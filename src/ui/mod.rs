//! The desktop user interface (egui / eframe).

pub mod account;
pub mod book;
pub mod settings;
pub mod song;
pub mod welcome;
pub mod widgets;

use crate::claude::RateLimit;
use crate::config::AppConfig;
use crate::jobs::{JobId, JobManager, JobMsg, JobOutput};
use crate::model::project::{Project, SongEntry, SongStatus};
use eframe::egui::{self, Color32, RichText};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum View {
    Home,
    Book,
    Inbox,
    Song(String),
    Account,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SongTab {
    Pages,
    Transcribe,
    Review,
    Details,
    Export,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ToastKind {
    Info,
    Success,
    Error,
}

pub struct Toast {
    pub text: String,
    pub kind: ToastKind,
    pub until: Instant,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobTag {
    Transcribe,
    Proofread,
    Fix,
    Detect,
    Book,
    Test,
    Export,
    Preview,
}

pub enum Confirm {
    DeleteSong(String),
    CloseProject,
    RetranscribeSong(String),
}

pub struct App {
    pub cfg: AppConfig,
    pub project: Option<Project>,
    dirty: bool,
    dirty_since: Instant,
    pub view: View,
    pub song_tab: SongTab,
    pub jobs: JobManager,
    pub job_tags: HashMap<JobId, JobTag>,
    pub tex: widgets::TextureCache,
    pub toasts: Vec<Toast>,
    pub auth: account::AuthUi,
    pub rate_limit: Option<RateLimit>,
    pub wizard: welcome::Wizard,
    pub search: String,
    pub song_ui: HashMap<String, song::SongUi>,
    pub book_ui: book::BookUi,
    pub show_settings: bool,
    pub show_help: bool,
    pub musescore: Option<PathBuf>,
    pub confirm: Option<Confirm>,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        widgets::install_fonts(&cc.egui_ctx);
        let cfg = AppConfig::load();
        apply_theme(&cc.egui_ctx, &cfg);
        let ctx = cc.egui_ctx.clone();
        let jobs = JobManager::new(Arc::new(move || ctx.request_repaint()));
        let musescore = crate::export::musescore::find(&cfg.musescore_path);
        let mut app = Self {
            auth: account::AuthUi::new(&cfg),
            cfg,
            project: None,
            dirty: false,
            dirty_since: Instant::now(),
            view: View::Home,
            song_tab: SongTab::Pages,
            jobs,
            job_tags: HashMap::new(),
            tex: Default::default(),
            toasts: Vec::new(),
            rate_limit: None,
            wizard: Default::default(),
            search: String::new(),
            song_ui: HashMap::new(),
            book_ui: Default::default(),
            show_settings: false,
            show_help: false,
            musescore,
            confirm: None,
        };
        app.auth.refresh(&app.cfg);
        // Reopen the last project.
        if let Some(last) = app.cfg.recent_projects.first().cloned() {
            if last.join(crate::model::project::PROJECT_FILE).exists() {
                app.open_project(&last);
            }
        }
        if !app.cfg.first_run_done {
            app.view = View::Account;
        }
        app
    }

    // ------------------------------------------------------------ helpers

    pub fn toast(&mut self, kind: ToastKind, text: impl Into<String>) {
        let secs = if kind == ToastKind::Error { 10 } else { 5 };
        self.toasts.push(Toast { text: text.into(), kind, until: Instant::now() + Duration::from_secs(secs) });
    }

    pub fn mark_dirty(&mut self) {
        if !self.dirty {
            self.dirty_since = Instant::now();
        }
        self.dirty = true;
    }

    pub fn save_now(&mut self) {
        if let Some(p) = &self.project {
            match p.save() {
                Ok(()) => self.dirty = false,
                Err(e) => {
                    self.dirty = false;
                    self.toast(ToastKind::Error, format!("Could not save the project: {e:#}"));
                }
            }
        }
    }

    pub fn save_config(&mut self) {
        if let Err(e) = self.cfg.save() {
            self.toast(ToastKind::Error, format!("Could not save settings: {e:#}"));
        }
    }

    pub fn open_project(&mut self, dir: &std::path::Path) {
        if self.dirty {
            self.save_now();
        }
        match Project::open(dir) {
            Ok(p) => {
                self.cfg.add_recent(p.dir.clone());
                self.save_config();
                self.view = if p.songs.len() == 1 && !p.kind.is_book() { View::Song(p.songs[0].id.clone()) } else { View::Book };
                self.project = Some(p);
                self.song_ui.clear();
                self.book_ui = Default::default();
            }
            Err(e) => self.toast(ToastKind::Error, format!("Could not open project: {e:#}")),
        }
    }

    pub fn close_project(&mut self) {
        self.jobs.cancel_all();
        if self.dirty {
            self.save_now();
        }
        self.project = None;
        self.view = View::Home;
        self.song_ui.clear();
    }

    pub fn entry(&self, id: &str) -> Option<&SongEntry> {
        self.project.as_ref().and_then(|p| p.song(id))
    }

    pub fn entry_mut(&mut self, id: &str) -> Option<&mut SongEntry> {
        self.project.as_mut().and_then(|p| p.song_mut(id))
    }

    pub fn add_song(&mut self) -> Option<String> {
        let p = self.project.as_mut()?;
        let e = SongEntry::default();
        let id = e.id.clone();
        p.songs.push(e);
        self.mark_dirty();
        self.view = View::Song(id.clone());
        self.song_tab = SongTab::Pages;
        Some(id)
    }

    pub fn submit(&mut self, tag: JobTag, title: impl Into<String>, kind: crate::jobs::JobKind) -> JobId {
        let song_id = kind.song_id().map(str::to_string);
        let id = self.jobs.submit(title, kind);
        self.job_tags.insert(id, tag);
        if let Some(sid) = song_id {
            if matches!(tag, JobTag::Transcribe | JobTag::Proofread | JobTag::Fix) {
                if let Some(e) = self.entry_mut(&sid) {
                    e.status = SongStatus::Queued;
                    e.last_error.clear();
                    if tag == JobTag::Transcribe {
                        e.log.clear();
                    }
                }
                self.mark_dirty();
            }
        }
        id
    }

    pub fn musescore_path(&self) -> Option<PathBuf> {
        self.musescore.clone()
    }

    /// Is the selected backend ready to use (signed in / key present)?
    pub fn backend_ready(&self) -> bool {
        self.auth.ready(&self.cfg)
    }

    // ------------------------------------------------------------ job results

    fn handle_jobs(&mut self) {
        let msgs = self.jobs.pump(&self.cfg);
        for (id, song_id, msg) in msgs {
            let tag = self.job_tags.get(&id).copied();
            match msg {
                JobMsg::Started => {
                    if let (Some(sid), Some(JobTag::Transcribe | JobTag::Proofread | JobTag::Fix)) = (&song_id, tag) {
                        if let Some(e) = self.entry_mut(sid) {
                            e.status = SongStatus::Working;
                        }
                    }
                }
                JobMsg::Status(_) | JobMsg::Streaming(_) => {}
                JobMsg::Log(line) => {
                    let stamp = chrono::Local::now().format("%H:%M:%S");
                    if let Some(sid) = &song_id {
                        if let Some(e) = self.entry_mut(sid) {
                            e.log.push(format!("{stamp}  {line}"));
                        }
                    } else {
                        self.book_ui.log.push(format!("{stamp}  {line}"));
                    }
                }
                JobMsg::RateLimit(r) => self.rate_limit = Some(r),
                JobMsg::Done(out) => {
                    self.job_tags.remove(&id);
                    self.on_done(tag, song_id, out);
                }
                JobMsg::Failed(err) => {
                    self.job_tags.remove(&id);
                    self.on_failed(tag, song_id, err);
                }
            }
        }
    }

    fn on_done(&mut self, tag: Option<JobTag>, song_id: Option<String>, out: JobOutput) {
        match out {
            JobOutput::Song(outcome) => {
                let Some(sid) = song_id else { return };
                let mut total_usage = None;
                if let Some(e) = self.entry_mut(&sid) {
                    for (label, s) in &outcome.revisions {
                        e.push_revision(label, s.clone());
                    }
                    if e.song.as_ref() != Some(&outcome.song) {
                        e.push_revision("Final result", outcome.song.clone());
                    }
                    e.issues = outcome.issues.clone();
                    e.usage.add(&outcome.usage);
                    e.status = SongStatus::NeedsReview;
                    e.last_error.clear();
                    for n in &outcome.notes {
                        if !n.is_empty() {
                            e.log.push(n.clone());
                        }
                    }
                    total_usage = Some(outcome.usage.clone());
                }
                if let Some(u) = total_usage {
                    if let Some(p) = self.project.as_mut() {
                        p.usage.add(&u);
                    }
                }
                let errors = crate::model::validate::error_count(&outcome.issues);
                let title = outcome.song.display_title();
                if errors == 0 {
                    self.toast(ToastKind::Success, format!("\"{title}\" is ready for review"));
                } else {
                    self.toast(ToastKind::Info, format!("\"{title}\" transcribed with {errors} remaining error(s) — please review"));
                }
                if let Some(ui) = self.song_ui.get_mut(&sid) {
                    ui.preview_pages.clear();
                    ui.json_text = None;
                }
                if self.view == View::Song(sid.clone())
                    && matches!(self.song_tab, SongTab::Transcribe | SongTab::Pages)
                    && tag == Some(JobTag::Transcribe)
                {
                    self.song_tab = SongTab::Review;
                }
                self.mark_dirty();
            }
            JobOutput::Detected(songs, usage) => {
                if let Some(p) = self.project.as_mut() {
                    p.usage.add(&usage);
                }
                let n = songs.len();
                self.book_ui.detected = Some(songs.into_iter().map(|s| (true, s)).collect());
                self.toast(ToastKind::Success, format!("Found {n} song(s) — review the list in the Inbox"));
                self.view = View::Inbox;
                self.mark_dirty();
            }
            JobOutput::Book(info, usage) => {
                if let Some(p) = self.project.as_mut() {
                    p.usage.add(&usage);
                    merge_book(&mut p.book, &info);
                    if !info.title.is_empty() && (p.name.is_empty() || p.name == "Untitled project") {
                        p.name = info.title.clone();
                    }
                }
                self.toast(ToastKind::Success, "Book details filled in from the photo — please check them");
                self.mark_dirty();
            }
            JobOutput::Text(t) => {
                let ok = t.to_ascii_uppercase().contains("READY");
                self.auth.test_result =
                    Some(if ok { Ok("Connection works — Claude answered.".to_string()) } else { Ok(format!("Claude answered: {t}")) });
            }
            JobOutput::Exported(results) => {
                let mut ok = 0;
                let mut files = Vec::new();
                for (title, r) in results {
                    match r {
                        Ok(res) => {
                            ok += 1;
                            for f in &res.files {
                                files.push(f.display().to_string());
                            }
                            for w in res.warnings {
                                self.book_ui.export_log.push(format!("{title}: {w}"));
                            }
                        }
                        Err(e) => {
                            self.book_ui.export_log.push(format!("{title}: FAILED — {e}"));
                            self.toast(ToastKind::Error, format!("Export of \"{title}\" failed: {e}"));
                        }
                    }
                }
                for f in &files {
                    self.book_ui.export_log.push(format!("Saved {f}"));
                }
                if ok > 0 {
                    self.toast(ToastKind::Success, format!("Exported {ok} song(s), {} file(s)", files.len()));
                }
            }
            JobOutput::Preview(pages) => {
                if let Some(sid) = song_id {
                    for p in &pages {
                        self.tex.invalidate(p);
                    }
                    self.song_ui.entry(sid).or_default().preview_pages = pages;
                }
            }
        }
    }

    fn on_failed(&mut self, tag: Option<JobTag>, song_id: Option<String>, err: String) {
        let cancelled = err.contains("Cancelled");
        if let Some(sid) = &song_id {
            if let Some(e) = self.entry_mut(sid) {
                if matches!(tag, Some(JobTag::Transcribe | JobTag::Proofread | JobTag::Fix)) {
                    e.status = if e.song.is_some() {
                        SongStatus::NeedsReview
                    } else if cancelled {
                        SongStatus::Draft
                    } else {
                        SongStatus::Failed
                    };
                    if !cancelled {
                        e.last_error = err.clone();
                    }
                    e.log.push(format!("Stopped: {err}"));
                }
            }
            self.mark_dirty();
        }
        match tag {
            Some(JobTag::Test) => self.auth.test_result = Some(Err(err.clone())),
            Some(JobTag::Preview) => {
                if let Some(sid) = &song_id {
                    self.song_ui.entry(sid.clone()).or_default().preview_error = Some(err.clone());
                }
            }
            _ => {}
        }
        if !cancelled && tag != Some(JobTag::Test) {
            self.toast(ToastKind::Error, err);
        }
    }

    // ------------------------------------------------------------ panels

    fn top_bar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("top").show(ctx, |ui| {
            egui::menu::bar(ui, |ui| {
                ui.menu_button("Project", |ui| {
                    if ui.button("New project…").clicked() {
                        self.wizard = Default::default();
                        self.wizard.open = true;
                        ui.close_menu();
                    }
                    if ui.button("Open project…").clicked() {
                        ui.close_menu();
                        if let Some(dir) = rfd::FileDialog::new().set_title("Open project folder").pick_folder() {
                            self.open_project(&dir);
                        }
                    }
                    ui.menu_button("Recent", |ui| {
                        let recents = self.cfg.recent_projects.clone();
                        if recents.is_empty() {
                            ui.label("No recent projects");
                        }
                        for r in recents {
                            if ui.button(r.display().to_string()).clicked() {
                                ui.close_menu();
                                self.open_project(&r);
                            }
                        }
                    });
                    ui.separator();
                    let has = self.project.is_some();
                    if ui.add_enabled(has, egui::Button::new("Save").shortcut_text("Ctrl+S")).clicked() {
                        self.save_now();
                        ui.close_menu();
                    }
                    if ui.add_enabled(has, egui::Button::new("Show project folder")).clicked() {
                        if let Some(p) = &self.project {
                            let _ = crate::export::musescore::open_with_default_app(&p.dir);
                        }
                        ui.close_menu();
                    }
                    if ui.add_enabled(has, egui::Button::new("Close project")).clicked() {
                        ui.close_menu();
                        self.close_project();
                    }
                });
                if self.project.is_some() {
                    ui.menu_button("Songs", |ui| {
                        if ui.button("➕ New song").clicked() {
                            ui.close_menu();
                            self.add_song();
                        }
                        if ui.button("Transcribe all drafts").clicked() {
                            ui.close_menu();
                            book::transcribe_all_drafts(self);
                        }
                        if ui.button("Export all transcribed songs").clicked() {
                            ui.close_menu();
                            book::export_all(self);
                        }
                        if ui.button("Sort songs by number").clicked() {
                            ui.close_menu();
                            if let Some(p) = self.project.as_mut() {
                                p.sort_songs();
                            }
                            self.mark_dirty();
                        }
                    });
                }
                ui.menu_button("View", |ui| {
                    if ui.checkbox(&mut self.cfg.dark_mode, "Dark mode").changed() {
                        apply_theme(ui.ctx(), &self.cfg);
                        self.save_config();
                    }
                    ui.horizontal(|ui| {
                        ui.label("Zoom");
                        if ui.add(egui::Slider::new(&mut self.cfg.ui_scale, 0.75..=1.75).step_by(0.05)).changed() {
                            apply_theme(ui.ctx(), &self.cfg);
                        }
                    });
                    if ui.button("Settings…").clicked() {
                        self.show_settings = true;
                        ui.close_menu();
                    }
                });
                ui.menu_button("Help", |ui| {
                    if ui.button("Getting started").clicked() {
                        self.show_help = true;
                        ui.close_menu();
                    }
                    if ui.button("Account & sign-in").clicked() {
                        self.view = View::Account;
                        ui.close_menu();
                    }
                });

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("⚙").on_hover_text("Settings").clicked() {
                        self.show_settings = true;
                    }
                    let ready = self.backend_ready();
                    let (dot, text) = if ready {
                        (Color32::from_rgb(60, 200, 110), self.auth.short_label(&self.cfg))
                    } else {
                        (Color32::from_rgb(230, 80, 80), "Sign in to Claude".to_string())
                    };
                    if ui
                        .add(egui::Button::new(RichText::new(format!("● {text}")).color(dot)))
                        .on_hover_text("Account and sign-in")
                        .clicked()
                    {
                        self.view = View::Account;
                    }
                    if let Some(rl) = &self.rate_limit {
                        if !rl.summary.is_empty() {
                            ui.label(RichText::new(&rl.summary).small().weak())
                                .on_hover_text("Claude subscription usage (from Claude Code)");
                        }
                    }
                });
            });
        });
    }

    fn status_bar(&mut self, ctx: &egui::Context) {
        let running: Vec<(JobId, String, String, usize, u64)> = self
            .jobs
            .running
            .iter()
            .map(|(id, r)| (*id, r.title.clone(), r.status.clone(), r.streamed, r.started.elapsed().as_secs()))
            .collect();
        let queued = self.jobs.queued().count();
        egui::TopBottomPanel::bottom("status").show(ctx, |ui| {
            ui.horizontal_wrapped(|ui| {
                if running.is_empty() && queued == 0 {
                    ui.label(RichText::new("Idle").weak());
                }
                let mut cancel = None;
                for (id, title, status, streamed, secs) in &running {
                    ui.spinner();
                    let mut s = format!("{title} — {status} ({}:{:02})", secs / 60, secs % 60);
                    if *streamed > 0 {
                        s.push_str(&format!(" · {} KB received", streamed / 1024));
                    }
                    ui.label(s);
                    if ui.small_button("Cancel").clicked() {
                        cancel = Some(*id);
                    }
                    ui.separator();
                }
                if let Some(id) = cancel {
                    self.jobs.cancel(id);
                }
                if queued > 0 {
                    ui.label(format!("{queued} queued"));
                    if ui.small_button("Clear queue").clicked() {
                        let ids: Vec<JobId> = self.jobs.queued().map(|j| j.id).collect();
                        let song_ids: Vec<String> = self.jobs.queued().filter_map(|j| j.kind.song_id().map(str::to_string)).collect();
                        for id in ids {
                            self.jobs.cancel(id);
                        }
                        for sid in song_ids {
                            if let Some(e) = self.entry_mut(&sid) {
                                e.status = if e.song.is_some() { SongStatus::NeedsReview } else { SongStatus::Draft };
                            }
                        }
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if let Some(p) = &self.project {
                        if p.usage.requests > 0 {
                            let mut s = format!("{} requests · {}k tokens out", p.usage.requests, p.usage.output_tokens / 1000);
                            if p.usage.cost_usd > 0.0 {
                                s.push_str(&format!(" · ≈${:.2} API-equivalent", p.usage.cost_usd));
                            }
                            ui.label(RichText::new(s).small().weak()).on_hover_text(
                                "Usage for this project. With a Claude subscription this is included in your plan; the dollar figure is what the same work would cost at API prices.",
                            );
                        }
                        if self.dirty {
                            ui.label(RichText::new("● unsaved").small().weak());
                        }
                    }
                });
            });
        });
    }

    fn side_panel(&mut self, ctx: &egui::Context) {
        if self.project.is_none() {
            return;
        }
        egui::SidePanel::left("songs").resizable(true).default_width(270.0).min_width(200.0).max_width(480.0).show(ctx, |ui| {
            let (name, kind, inbox_len) = {
                let p = self.project.as_ref().unwrap();
                let name = if p.book.title.is_empty() { p.name.clone() } else { p.book.title.clone() };
                (name, p.kind, p.inbox.len())
            };
            ui.add_space(4.0);
            if ui
                .selectable_label(self.view == View::Book, RichText::new(format!("📖 {name}")).strong())
                .on_hover_text(format!("{} — details, batch actions and exports", kind.label()))
                .clicked()
            {
                self.view = View::Book;
            }
            if kind.is_book() {
                let label =
                    if inbox_len > 0 { format!("📥 Page inbox ({inbox_len})") } else { "📥 Page inbox (auto-detect songs)".into() };
                if ui.selectable_label(self.view == View::Inbox, label).clicked() {
                    self.view = View::Inbox;
                }
            }
            ui.separator();
            ui.horizontal(|ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut self.search)
                        .hint_text("🔍 Search songs")
                        .desired_width((ui.available_width() - 48.0).max(80.0)),
                );
                if ui.button("➕").on_hover_text("New song").clicked() {
                    self.add_song();
                }
            });
            ui.add_space(4.0);
            let q = self.search.to_lowercase();
            let mut clicked = None;
            let mut delete = None;
            egui::ScrollArea::vertical().id_salt("song_list").show(ui, |ui| {
                let p = self.project.as_ref().unwrap();
                if p.songs.is_empty() {
                    ui.label(
                        RichText::new("No songs yet. Click ➕ to add a song, or use the page inbox to detect songs automatically.").weak(),
                    );
                }
                for e in &p.songs {
                    let title = e.display_title();
                    let num = e.display_number();
                    if !q.is_empty() {
                        let hay = format!(
                            "{} {} {} {}",
                            title.to_lowercase(),
                            num.to_lowercase(),
                            e.song.as_ref().map(|s| s.meta.first_line.to_lowercase()).unwrap_or_default(),
                            e.song.as_ref().map(|s| s.meta.tune_name.to_lowercase()).unwrap_or_default()
                        );
                        if !hay.contains(&q) {
                            continue;
                        }
                    }
                    let busy = self.jobs.is_busy(&e.id);
                    let icon = if busy && e.status != SongStatus::Queued { "⟳" } else { e.status.icon() };
                    let label = if num.is_empty() { format!("{icon}  {title}") } else { format!("{icon}  {num}. {title}") };
                    let selected = self.view == View::Song(e.id.clone());
                    let resp = ui.selectable_label(selected, label).on_hover_text(format!(
                        "{} · {} page(s){}",
                        e.status.label(),
                        e.pages.len(),
                        if e.issues.is_empty() { String::new() } else { format!(" · {} issue(s)", e.issues.len()) }
                    ));
                    if resp.clicked() {
                        clicked = Some(e.id.clone());
                    }
                    resp.context_menu(|ui| {
                        if ui.button("Delete song…").clicked() {
                            delete = Some(e.id.clone());
                            ui.close_menu();
                        }
                    });
                }
            });
            if let Some(id) = clicked {
                if self.view != View::Song(id.clone()) {
                    let has_song = self.entry(&id).map(|e| e.song.is_some()).unwrap_or(false);
                    self.song_tab = if has_song { SongTab::Review } else { SongTab::Pages };
                }
                self.view = View::Song(id);
            }
            if let Some(id) = delete {
                self.confirm = Some(Confirm::DeleteSong(id));
            }
        });
    }

    fn toasts_ui(&mut self, ctx: &egui::Context) {
        let now = Instant::now();
        self.toasts.retain(|t| t.until > now);
        if self.toasts.is_empty() {
            return;
        }
        egui::Area::new(egui::Id::new("toasts"))
            .anchor(egui::Align2::RIGHT_BOTTOM, egui::vec2(-12.0, -40.0))
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                let mut close = None;
                for (i, t) in self.toasts.iter().enumerate().rev().take(5) {
                    let color = match t.kind {
                        ToastKind::Info => Color32::from_rgb(70, 130, 220),
                        ToastKind::Success => Color32::from_rgb(50, 160, 90),
                        ToastKind::Error => Color32::from_rgb(200, 60, 60),
                    };
                    egui::Frame::popup(ui.style()).stroke(egui::Stroke::new(1.5_f32, color)).show(ui, |ui| {
                        ui.set_max_width(420.0);
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("●").color(color));
                            ui.label(&t.text);
                            if ui.small_button("✕").clicked() {
                                close = Some(i);
                            }
                        });
                    });
                    ui.add_space(4.0);
                }
                if let Some(i) = close {
                    self.toasts.remove(i);
                }
            });
        ctx.request_repaint_after(Duration::from_millis(500));
    }

    fn confirm_ui(&mut self, ctx: &egui::Context) {
        let Some(c) = &self.confirm else { return };
        let (title, text) = match c {
            Confirm::DeleteSong(id) => (
                "Delete song?",
                format!(
                    "Delete \"{}\" and its transcription history? Page images stay in the project folder.",
                    self.entry(id).map(|e| e.display_title()).unwrap_or_default()
                ),
            ),
            Confirm::CloseProject => ("Close project?", "Running jobs will be cancelled.".to_string()),
            Confirm::RetranscribeSong(id) => (
                "Transcribe again?",
                format!(
                    "\"{}\" already has a transcription. Transcribing again replaces it (the current version stays in History).",
                    self.entry(id).map(|e| e.display_title()).unwrap_or_default()
                ),
            ),
        };
        let mut result = None;
        egui::Window::new(title).collapsible(false).resizable(false).anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO).show(
            ctx,
            |ui| {
                ui.label(text);
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("Cancel").clicked() {
                        result = Some(false);
                    }
                    if ui.button(RichText::new("OK").strong()).clicked() {
                        result = Some(true);
                    }
                });
            },
        );
        match result {
            Some(true) => match self.confirm.take() {
                Some(Confirm::DeleteSong(id)) => {
                    self.jobs.cancel_song(&id);
                    if let Some(p) = self.project.as_mut() {
                        p.songs.retain(|s| s.id != id);
                    }
                    if self.view == View::Song(id) {
                        self.view = View::Book;
                    }
                    self.mark_dirty();
                }
                Some(Confirm::CloseProject) => self.close_project(),
                Some(Confirm::RetranscribeSong(id)) => song::start_transcription(self, &id),
                None => {}
            },
            Some(false) => self.confirm = None,
            None => {}
        }
    }

    fn handle_dropped_files(&mut self, ctx: &egui::Context) {
        let dropped: Vec<PathBuf> = ctx.input(|i| i.raw.dropped_files.iter().filter_map(|f| f.path.clone()).collect());
        if dropped.is_empty() {
            return;
        }
        let images: Vec<PathBuf> = dropped.into_iter().filter(|p| crate::imaging::is_supported(p)).collect();
        if images.is_empty() {
            self.toast(ToastKind::Error, "Drop JPEG, PNG, WebP, TIFF or BMP images.");
            return;
        }
        if self.project.is_none() {
            self.toast(ToastKind::Info, "Create or open a project first, then drop page images onto a song.");
            return;
        }
        match self.view.clone() {
            View::Inbox => book::add_inbox_pages(self, &images),
            View::Song(id) => song::add_pages(self, &id, &images),
            _ => {
                if let Some(id) = self.add_song() {
                    song::add_pages(self, &id, &images);
                }
            }
        }
    }
}

fn merge_book(dst: &mut crate::model::project::BookInfo, src: &crate::model::project::BookInfo) {
    macro_rules! m {
        ($($f:ident),*) => { $( if !src.$f.trim().is_empty() { dst.$f = src.$f.trim().to_string(); } )* };
    }
    m!(title, subtitle, abbreviation, publisher, year, edition, editor, denomination, language, isbn, copyright, notes);
}

pub fn apply_theme(ctx: &egui::Context, cfg: &AppConfig) {
    let mut visuals = if cfg.dark_mode { egui::Visuals::dark() } else { egui::Visuals::light() };
    visuals.selection.bg_fill = Color32::from_rgb(70, 110, 190);
    ctx.set_visuals(visuals);
    ctx.set_zoom_factor(cfg.ui_scale.clamp(0.5, 2.5));
    ctx.style_mut(|s| {
        s.spacing.item_spacing = egui::vec2(8.0, 6.0);
        s.spacing.button_padding = egui::vec2(8.0, 4.0);
    });
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.tex.pump(ctx);
        self.handle_jobs();
        self.auth.poll(ctx, &self.cfg);
        self.handle_dropped_files(ctx);

        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::S)) {
            self.save_now();
        }
        // Save shortly after edits; while jobs run, save less often.
        let delay = if self.jobs.running.is_empty() { Duration::from_millis(1500) } else { Duration::from_secs(10) };
        if self.dirty && self.dirty_since.elapsed() > delay {
            self.save_now();
        }

        self.top_bar(ctx);
        self.status_bar(ctx);
        self.side_panel(ctx);
        egui::CentralPanel::default().show(ctx, |ui| match self.view.clone() {
            View::Home => welcome::home(self, ui),
            View::Account => account::account_view(self, ui),
            View::Book => {
                if self.project.is_some() {
                    book::book_view(self, ui)
                } else {
                    welcome::home(self, ui)
                }
            }
            View::Inbox => {
                if self.project.is_some() {
                    book::inbox_view(self, ui)
                } else {
                    welcome::home(self, ui)
                }
            }
            View::Song(id) => {
                if self.entry(&id).is_some() {
                    song::song_view(self, ui, &id)
                } else {
                    self.view = View::Book;
                }
            }
        });
        welcome::wizard_window(self, ctx);
        settings::settings_window(self, ctx);
        welcome::help_window(self, ctx);
        self.confirm_ui(ctx);
        self.toasts_ui(ctx);

        if !self.jobs.is_idle() {
            ctx.request_repaint_after(Duration::from_millis(500));
        }
    }

    fn on_exit(&mut self) {
        self.jobs.cancel_all();
        if self.dirty {
            if let Some(p) = &self.project {
                let _ = p.save();
            }
        }
        let _ = self.cfg.save();
    }
}
