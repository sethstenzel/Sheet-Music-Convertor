//! Background jobs (Claude requests, exports, previews) with a small queue.

use crate::claude::{backend_from_config, Cancel, RateLimit};
use crate::config::AppConfig;
use crate::export::{ExportResult, ExportSettings, MusicXmlOptions};
use crate::model::project::{BookInfo, CollectionKind, Project, SongEntry, SongPage, SourceInfo, Usage};
use crate::model::score::Song;
use crate::pipeline::{self, DetectedSong, Env, Event, Outcome, SongInput};
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;

pub type JobId = u64;

#[derive(Clone, Debug)]
pub enum JobKind {
    Transcribe { song_id: String, input: SongInput },
    Proofread { song_id: String, input: SongInput },
    UserFix { song_id: String, input: SongInput, request: String },
    DetectSongs { kind: CollectionKind, book: BookInfo, images_dir: PathBuf, pages: Vec<SongPage> },
    BookInfo { paths: Vec<PathBuf> },
    TestConnection,
    Export { project: Box<Project>, song_ids: Vec<String>, settings: ExportSettings, musescore: Option<PathBuf> },
    Preview { song_id: String, song: Box<Song>, source: SourceInfo, xml: MusicXmlOptions, transpose: i32, musescore: PathBuf, dir: PathBuf },
}

impl JobKind {
    pub fn song_id(&self) -> Option<&str> {
        match self {
            JobKind::Transcribe { song_id, .. }
            | JobKind::Proofread { song_id, .. }
            | JobKind::UserFix { song_id, .. }
            | JobKind::Preview { song_id, .. } => Some(song_id),
            _ => None,
        }
    }

    /// Jobs that call Claude count against the parallel limit.
    fn uses_claude(&self) -> bool {
        !matches!(self, JobKind::Export { .. } | JobKind::Preview { .. })
    }
}

#[derive(Debug)]
pub enum JobOutput {
    Song(Outcome),
    Detected(Vec<DetectedSong>, Usage),
    Book(BookInfo, Usage),
    Text(String),
    Exported(Vec<(String, Result<ExportResult, String>)>),
    Preview(Vec<PathBuf>),
}

#[derive(Debug)]
pub enum JobMsg {
    Started,
    Status(String),
    Log(String),
    Streaming(usize),
    RateLimit(RateLimit),
    Done(JobOutput),
    Failed(String),
}

pub struct Job {
    pub id: JobId,
    pub title: String,
    pub kind: JobKind,
}

pub struct Running {
    pub title: String,
    pub song_id: Option<String>,
    pub status: String,
    pub streamed: usize,
    pub started: std::time::Instant,
    pub cancel: Cancel,
    uses_claude: bool,
}

pub struct JobManager {
    next_id: JobId,
    queue: VecDeque<Job>,
    pub running: HashMap<JobId, Running>,
    tx: Sender<(JobId, JobMsg)>,
    rx: Receiver<(JobId, JobMsg)>,
    notify: Arc<dyn Fn() + Send + Sync>,
}

impl JobManager {
    pub fn new(notify: Arc<dyn Fn() + Send + Sync>) -> Self {
        let (tx, rx) = channel();
        Self { next_id: 1, queue: VecDeque::new(), running: HashMap::new(), tx, rx, notify }
    }

    pub fn submit(&mut self, title: impl Into<String>, kind: JobKind) -> JobId {
        let id = self.next_id;
        self.next_id += 1;
        self.queue.push_back(Job { id, title: title.into(), kind });
        id
    }

    pub fn queued(&self) -> impl Iterator<Item = &Job> {
        self.queue.iter()
    }

    pub fn is_busy(&self, song_id: &str) -> bool {
        self.running.values().any(|r| r.song_id.as_deref() == Some(song_id)) || self.queue.iter().any(|j| j.kind.song_id() == Some(song_id))
    }

    pub fn is_idle(&self) -> bool {
        self.queue.is_empty() && self.running.is_empty()
    }

    pub fn cancel(&mut self, id: JobId) {
        self.queue.retain(|j| j.id != id);
        if let Some(r) = self.running.get(&id) {
            r.cancel.cancel();
        }
    }

    pub fn cancel_song(&mut self, song_id: &str) -> Vec<JobId> {
        let mut ids: Vec<JobId> = self.queue.iter().filter(|j| j.kind.song_id() == Some(song_id)).map(|j| j.id).collect();
        self.queue.retain(|j| j.kind.song_id() != Some(song_id));
        for (id, r) in &self.running {
            if r.song_id.as_deref() == Some(song_id) {
                r.cancel.cancel();
                ids.push(*id);
            }
        }
        ids
    }

    pub fn cancel_all(&mut self) {
        self.queue.clear();
        for r in self.running.values() {
            r.cancel.cancel();
        }
    }

    /// Start queued jobs and collect messages. Call every frame.
    pub fn pump(&mut self, cfg: &AppConfig) -> Vec<(JobId, Option<String>, JobMsg)> {
        let limit = cfg.parallel_jobs.max(1) as usize;
        let mut i = 0;
        while i < self.queue.len() {
            let claude_running = self.running.values().filter(|r| r.uses_claude).count();
            let job = &self.queue[i];
            let busy_song = job.kind.song_id().map(|s| self.running.values().any(|r| r.song_id.as_deref() == Some(s))).unwrap_or(false);
            if busy_song || (job.kind.uses_claude() && claude_running >= limit) {
                i += 1;
                continue;
            }
            let job = self.queue.remove(i).expect("index checked");
            self.start(job, cfg.clone());
        }
        let mut out = Vec::new();
        while let Ok((id, msg)) = self.rx.try_recv() {
            let song_id = self.running.get(&id).and_then(|r| r.song_id.clone());
            if let Some(r) = self.running.get_mut(&id) {
                match &msg {
                    JobMsg::Status(s) => r.status = s.clone(),
                    JobMsg::Streaming(n) => r.streamed = *n,
                    _ => {}
                }
            }
            if matches!(msg, JobMsg::Done(_) | JobMsg::Failed(_)) {
                self.running.remove(&id);
            }
            out.push((id, song_id, msg));
        }
        out
    }

    fn start(&mut self, job: Job, cfg: AppConfig) {
        let cancel = Cancel::default();
        self.running.insert(
            job.id,
            Running {
                title: job.title.clone(),
                song_id: job.kind.song_id().map(str::to_string),
                status: "Starting…".into(),
                streamed: 0,
                started: std::time::Instant::now(),
                cancel: cancel.clone(),
                uses_claude: job.kind.uses_claude(),
            },
        );
        let tx = self.tx.clone();
        let notify = self.notify.clone();
        let id = job.id;
        std::thread::spawn(move || {
            let send = |m: JobMsg| {
                let _ = tx.send((id, m));
                notify();
            };
            send(JobMsg::Started);
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run(job.kind, &cfg, &cancel, &send)));
            match result {
                Ok(Ok(out)) => send(JobMsg::Done(out)),
                Ok(Err(e)) => send(JobMsg::Failed(format!("{e:#}"))),
                Err(p) => {
                    let msg = p
                        .downcast_ref::<String>()
                        .cloned()
                        .or_else(|| p.downcast_ref::<&str>().map(|s| s.to_string()))
                        .unwrap_or_else(|| "unknown error".into());
                    send(JobMsg::Failed(format!("Internal error: {msg}")))
                }
            }
        });
    }
}

fn run(kind: JobKind, cfg: &AppConfig, cancel: &Cancel, send: &dyn Fn(JobMsg)) -> anyhow::Result<JobOutput> {
    let backend = backend_from_config(cfg);
    let env = Env { backend: backend.as_ref(), cfg, cancel };
    let mut emit = |e: Event| match e {
        Event::Status(s) => send(JobMsg::Status(s)),
        Event::Log(s) => send(JobMsg::Log(s)),
        Event::Streaming(n) => send(JobMsg::Streaming(n)),
        Event::RateLimit(r) => send(JobMsg::RateLimit(r)),
    };
    match kind {
        JobKind::Transcribe { input, .. } => Ok(JobOutput::Song(pipeline::transcribe(&env, &input, &mut emit)?)),
        JobKind::Proofread { input, .. } => Ok(JobOutput::Song(pipeline::proofread(&env, &input, &mut emit)?)),
        JobKind::UserFix { input, request, .. } => Ok(JobOutput::Song(pipeline::user_fix(&env, &input, &request, &mut emit)?)),
        JobKind::DetectSongs { kind, book, images_dir, pages } => {
            let (songs, usage) = pipeline::detect_songs(&env, kind, &book, &images_dir, &pages, &mut emit)?;
            Ok(JobOutput::Detected(songs, usage))
        }
        JobKind::BookInfo { paths } => {
            let (info, usage) = pipeline::read_book_info(&env, &paths, &mut emit)?;
            Ok(JobOutput::Book(info, usage))
        }
        JobKind::TestConnection => Ok(JobOutput::Text(pipeline::test_connection(&env, &mut emit)?)),
        JobKind::Export { project, song_ids, settings, musescore } => {
            let mut results = Vec::new();
            for (n, id) in song_ids.iter().enumerate() {
                if cancel.is_cancelled() {
                    anyhow::bail!("Cancelled");
                }
                let Some(entry): Option<&SongEntry> = project.song(id) else { continue };
                send(JobMsg::Status(format!("Exporting {} of {}: {}", n + 1, song_ids.len(), entry.display_title())));
                let r = crate::export::export_song(&project, entry, &settings, musescore.as_deref()).map_err(|e| format!("{e:#}"));
                results.push((entry.display_title(), r));
            }
            Ok(JobOutput::Exported(results))
        }
        JobKind::Preview { song, source, xml, transpose, musescore, dir, .. } => {
            send(JobMsg::Status("Rendering notation with MuseScore…".into()));
            let parsed = crate::export::prepare(&song, transpose);
            Ok(JobOutput::Preview(crate::export::render_preview(&parsed, &source, &xml, &musescore, &dir)?))
        }
    }
}
