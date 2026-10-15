//! Multi-pass transcription: transcribe → validate → auto-fix → proofread.

use crate::claude::extract::{extract_json, song_from_answer};
use crate::claude::prompts::{self, PageContext, PageImages, SongContext};
use crate::claude::{Backend, Cancel, ImageInput, Part, Progress, RateLimit, Request};
use crate::config::{Accuracy, AppConfig};
use crate::imaging;
use crate::model::project::{BookInfo, CollectionKind, SongPage, Usage};
use crate::model::score::{Measure, Song};
use crate::model::validate::{error_count, validate, Issue, Severity};
use anyhow::{anyhow, bail, Result};
use serde::Deserialize;
use std::path::PathBuf;

#[derive(Clone, Debug)]
pub enum Event {
    Status(String),
    Log(String),
    Streaming(usize),
    RateLimit(RateLimit),
}

pub struct Env<'a> {
    pub backend: &'a dyn Backend,
    pub cfg: &'a AppConfig,
    pub cancel: &'a Cancel,
}

/// Everything needed to work on one song, captured from the project.
#[derive(Clone, Debug)]
pub struct SongInput {
    pub kind: CollectionKind,
    pub book: BookInfo,
    pub images_dir: PathBuf,
    pub pages: Vec<SongPage>,
    pub title_hint: String,
    pub number_hint: String,
    pub instructions: String,
    pub current: Option<Song>,
}

#[derive(Clone, Debug, Default)]
pub struct Outcome {
    pub song: Song,
    pub issues: Vec<Issue>,
    pub usage: Usage,
    /// Intermediate versions worth keeping in history: (label, song).
    pub revisions: Vec<(String, Song)>,
    pub notes: Vec<String>,
}

const TRANSCRIBE_TOKENS: u32 = 64_000;
const PATCH_TOKENS: u32 = 48_000;

impl SongInput {
    fn context(&self, cfg: &AppConfig) -> SongContext {
        SongContext {
            kind: self.kind,
            book: self.book.clone(),
            title_hint: self.title_hint.clone(),
            number_hint: self.number_hint.clone(),
            instructions: self.instructions.clone(),
            layout: cfg.voice_layout,
            pages: self
                .pages
                .iter()
                .map(|p| PageContext {
                    book_page: p.book_page.clone(),
                    ignore_top: p.ignore_top,
                    ignore_bottom: p.ignore_bottom,
                    note: p.note.clone(),
                    cropped: p.crop.is_some(),
                })
                .collect(),
        }
    }

    fn images(&self, cfg: &AppConfig, tiles: bool, emit: &mut dyn FnMut(Event)) -> Result<Vec<PageImages>> {
        if self.pages.is_empty() {
            bail!("Add at least one page image to this song first.");
        }
        let mut out = Vec::new();
        for (i, p) in self.pages.iter().enumerate() {
            emit(Event::Status(format!("Preparing page {} of {}…", i + 1, self.pages.len())));
            out.push(imaging::prepare_page(&self.images_dir, p, cfg.max_image_dim, tiles)?);
        }
        Ok(out)
    }
}

fn call(
    env: &Env,
    label: &str,
    system: String,
    parts: Vec<Part>,
    max_tokens: u32,
    usage: &mut Usage,
    emit: &mut dyn FnMut(Event),
) -> Result<String> {
    if env.cancel.is_cancelled() {
        bail!("Cancelled");
    }
    let req = Request { system, parts, label: label.to_string(), max_output_tokens: max_tokens };
    emit(Event::Log(format!("→ {label} ({})", env.backend.name())));
    let started = std::time::Instant::now();
    let resp = env.backend.send(&req, env.cancel, &mut |p| match p {
        Progress::Status(s) => emit(Event::Status(format!("{label}: {s}"))),
        Progress::Streaming(n) => emit(Event::Streaming(n)),
        Progress::RateLimit(r) => emit(Event::RateLimit(r)),
    })?;
    usage.add(&resp.usage);
    emit(Event::Log(format!(
        "← {label}: {} output tokens in {:.0}s{}",
        resp.usage.output_tokens,
        started.elapsed().as_secs_f32(),
        if resp.usage.cost_usd > 0.0 { format!(" (≈${:.3} API-equivalent)", resp.usage.cost_usd) } else { String::new() }
    )));
    if resp.stop_reason == "max_tokens" {
        emit(Event::Log("Warning: the answer hit the output length limit and may be cut off.".into()));
    }
    Ok(resp.text)
}

/// Call and parse a song (or patch), retrying once if the JSON is malformed.
fn call_for_song(
    env: &Env,
    label: &str,
    system: &str,
    parts: Vec<Part>,
    max_tokens: u32,
    current: Option<&Song>,
    usage: &mut Usage,
    emit: &mut dyn FnMut(Event),
) -> Result<(Song, String)> {
    let text = call(env, label, system.to_string(), parts.clone(), max_tokens, usage, emit)?;
    match song_from_answer(&text, current) {
        Ok(r) => Ok(r),
        Err(e) => {
            emit(Event::Log(format!("Could not read Claude's answer ({e:#}); asking again.")));
            let mut retry = parts;
            retry.push(Part::Text(format!(
                "IMPORTANT: a previous attempt at this answer could not be used ({e}). Reply with one complete, valid \
                 JSON object in a ```json block, without comments or trailing commas."
            )));
            let text = call(env, &format!("{label} (retry)"), system.to_string(), retry, max_tokens, usage, emit)?;
            song_from_answer(&text, current).map_err(|e| anyhow!("Claude's answer could not be read: {e:#}"))
        }
    }
}

fn apply_hints(song: &mut Song, input: &SongInput) {
    if song.meta.number.trim().is_empty() && !input.number_hint.trim().is_empty() {
        song.meta.number = input.number_hint.trim().to_string();
    }
    if song.meta.title.trim().is_empty() && !input.title_hint.trim().is_empty() {
        song.meta.title = input.title_hint.trim().to_string();
    }
    if song.verse_count == 0 {
        song.verse_count = song.max_verse().max(1);
    }
}

fn describe_issues(issues: &[Issue]) -> String {
    let e = error_count(issues);
    let w = issues.len() - e;
    format!("{e} error(s), {w} warning(s)")
}

/// Repeatedly ask Claude to fix validator errors. Keeps the best version.
fn fix_loop(
    env: &Env,
    ctx: &SongContext,
    images: &[PageImages],
    song: Song,
    usage: &mut Usage,
    revisions: &mut Vec<(String, Song)>,
    emit: &mut dyn FnMut(Event),
) -> Result<(Song, Vec<Issue>)> {
    let system = prompts::transcription_system();
    let (_, mut issues) = validate(&song);
    let mut best = (song, issues.clone());
    let mut current = best.0.clone();
    for round in 1..=env.cfg.max_fix_rounds {
        let errors: Vec<Issue> = issues.iter().filter(|i| i.severity == Severity::Error).cloned().collect();
        if errors.is_empty() {
            break;
        }
        emit(Event::Status(format!("Fixing {} problem(s), round {round}…", errors.len())));
        let parts = prompts::fix_errors(ctx, images, &current, &errors);
        let (fixed, summary) = call_for_song(env, &format!("Auto-fix {round}"), &system, parts, PATCH_TOKENS, Some(&current), usage, emit)?;
        if !summary.is_empty() {
            emit(Event::Log(format!("Auto-fix {round}: {summary}")));
        }
        let (_, new_issues) = validate(&fixed);
        emit(Event::Log(format!("After auto-fix {round}: {}", describe_issues(&new_issues))));
        current = fixed;
        issues = new_issues;
        revisions.push((format!("Auto-fix {round}"), current.clone()));
        if error_count(&issues) <= error_count(&best.1) {
            best = (current.clone(), issues.clone());
        }
    }
    Ok(best)
}

fn log_transcribed(song: &Song, emit: &mut dyn FnMut(Event)) {
    let (_, issues) = validate(song);
    emit(Event::Log(format!(
        "Transcribed \"{}\": {} measures, {} verse(s); {}",
        song.display_title(),
        song.measures.len(),
        song.verse_count,
        describe_issues(&issues)
    )));
}

/// A measure with whitespace normalized, for comparing two readings.
fn normalized(m: &Measure) -> Measure {
    let mut m = m.clone();
    for vm in m.voices.values_mut() {
        vm.notes = vm.notes.split_whitespace().collect::<Vec<_>>().join(" ");
        for t in vm.lyrics.values_mut() {
            *t = t.split_whitespace().collect::<Vec<_>>().join(" ");
        }
    }
    m
}

fn compact_measure_json(m: &Measure) -> String {
    let s = Song { measures: vec![m.clone()], ..Default::default() };
    let j = crate::claude::extract::song_json_with_indices(&s);
    let v: serde_json::Value = serde_json::from_str(&j).unwrap_or_default();
    let mut mv = v["measures"][0].clone();
    if let Some(o) = mv.as_object_mut() {
        o.remove("i");
    }
    mv.to_string()
}

type Reading = (Result<(Song, String)>, Usage, Vec<Event>);

/// Maximum accuracy: two independent readings in parallel (full page and
/// zoomed tiles); the measures where they disagree are settled against the image.
#[allow(clippy::too_many_arguments)]
fn ensemble_transcription(
    env: &Env,
    input: &SongInput,
    ctx: &SongContext,
    tiled: &[PageImages],
    usage: &mut Usage,
    revisions: &mut Vec<(String, Song)>,
    notes: &mut Vec<String>,
    emit: &mut dyn FnMut(Event),
) -> Result<Song> {
    let system = prompts::transcription_system();
    let full: Vec<PageImages> = tiled.iter().map(|p| PageImages { full: p.full.clone(), tiles: Vec::new() }).collect();
    emit(Event::Status("Two independent transcriptions in progress…".into()));
    let run = |label: &str, imgs: &[PageImages]| -> Reading {
        let mut u = Usage::default();
        let mut events = Vec::new();
        let parts = prompts::transcribe(ctx, imgs);
        let r = call_for_song(env, label, &system, parts, TRANSCRIBE_TOKENS, None, &mut u, &mut |e| events.push(e));
        (r, u, events)
    };
    let (ra, rb) = std::thread::scope(|s| {
        let ha = s.spawn(|| run("Reading A (full page)", &full));
        let hb = s.spawn(|| run("Reading B (zoomed)", tiled));
        (ha.join(), hb.join())
    });
    let mut results = Vec::new();
    for r in [ra, rb] {
        let (res, u, events) = r.map_err(|_| anyhow!("a transcription thread crashed"))?;
        usage.add(&u);
        for e in events {
            if !matches!(e, Event::Streaming(_)) {
                emit(e);
            }
        }
        results.push(res);
    }
    let rb = results.pop().expect("two results");
    let ra = results.pop().expect("two results");
    let (mut a, mut b) = match (ra, rb) {
        (Ok((a, _)), Ok((b, _))) => (a, b),
        (Ok((mut one, _)), Err(e)) | (Err(e), Ok((mut one, _))) => {
            emit(Event::Log(format!("One reading failed ({e:#}); continuing with the other.")));
            apply_hints(&mut one, input);
            revisions.push(("Claude transcription".into(), one.clone()));
            return Ok(one);
        }
        (Err(e), Err(_)) => return Err(e),
    };
    apply_hints(&mut a, input);
    apply_hints(&mut b, input);
    log_transcribed(&a, emit);
    log_transcribed(&b, emit);
    revisions.push(("Reading A (full page)".into(), a.clone()));
    revisions.push(("Reading B (zoomed)".into(), b.clone()));
    // Start from the reading with fewer validator errors (the zoomed one on ties).
    let (ea, eb) = (error_count(&validate(&a).1), error_count(&validate(&b).1));
    let (primary, other) = if ea < eb { (a, b) } else { (b, a) };
    if primary.measures.len() != other.measures.len() {
        let msg = format!(
            "The two readings found different numbers of measures ({} vs {}); continuing with the better one.",
            primary.measures.len(),
            other.measures.len()
        );
        emit(Event::Log(msg.clone()));
        notes.push(msg);
        return Ok(primary);
    }
    let disagreements: Vec<(usize, String, String)> = primary
        .measures
        .iter()
        .zip(&other.measures)
        .enumerate()
        .filter(|(_, (x, y))| normalized(x) != normalized(y))
        .map(|(i, (x, y))| (i, compact_measure_json(x), compact_measure_json(y)))
        .collect();
    if disagreements.is_empty() {
        emit(Event::Log("Both readings agree on every measure.".into()));
        return Ok(primary);
    }
    emit(Event::Log(format!(
        "The readings disagree in {} of {} measures; settling them against the image.",
        disagreements.len(),
        primary.measures.len()
    )));
    emit(Event::Status(format!("Comparing {} disagreeing measure(s) with the image…", disagreements.len())));
    let parts = prompts::adjudicate(ctx, tiled, &primary, &disagreements);
    let (settled, summary) = call_for_song(env, "Comparing readings", &system, parts, PATCH_TOKENS, Some(&primary), usage, emit)?;
    if !summary.is_empty() {
        emit(Event::Log(format!("Comparing readings: {summary}")));
        notes.push(format!("Comparing readings: {summary}"));
    }
    revisions.push(("Readings compared".into(), settled.clone()));
    Ok(settled)
}

/// Full transcription of a song from its pages.
pub fn transcribe(env: &Env, input: &SongInput, emit: &mut dyn FnMut(Event)) -> Result<Outcome> {
    let cfg = env.cfg;
    let ctx = input.context(cfg);
    // Prepare each page once; the first pass may leave out the zoomed tiles.
    let images = input.images(cfg, cfg.accuracy.detail_in_review() || cfg.accuracy.detail_in_transcription(), emit)?;
    let first_images: Vec<PageImages> = if cfg.accuracy.detail_in_transcription() {
        images.clone()
    } else {
        images.iter().map(|p| PageImages { full: p.full.clone(), tiles: Vec::new() }).collect()
    };
    let mut usage = Usage::default();
    let mut revisions = Vec::new();
    let mut notes = Vec::new();
    let system = prompts::transcription_system();

    let mut song = if cfg.accuracy == Accuracy::Maximum {
        ensemble_transcription(env, input, &ctx, &images, &mut usage, &mut revisions, &mut notes, emit)?
    } else {
        emit(Event::Status("Transcribing…".into()));
        let parts = prompts::transcribe(&ctx, &first_images);
        let (song, _) = call_for_song(env, "Transcription", &system, parts, TRANSCRIBE_TOKENS, None, &mut usage, emit)?;
        let mut song = song;
        apply_hints(&mut song, input);
        log_transcribed(&song, emit);
        revisions.push(("Claude transcription".to_string(), song.clone()));
        song
    };
    apply_hints(&mut song, input);

    let (mut song, mut issues) = fix_loop(env, &ctx, &images, song, &mut usage, &mut revisions, emit)?;

    for round in 1..=cfg.accuracy.proofread_passes() {
        emit(Event::Status(format!("Proofreading against the pages (pass {round})…")));
        let warnings: Vec<Issue> = issues.iter().filter(|i| i.severity == Severity::Warning).cloned().collect();
        let parts = prompts::proofread(&ctx, &images, &song, round, &warnings);
        let (checked, summary) =
            call_for_song(env, &format!("Proofreading {round}"), &system, parts, PATCH_TOKENS, Some(&song), &mut usage, emit)?;
        if !summary.is_empty() {
            emit(Event::Log(format!("Proofreading {round}: {summary}")));
            notes.push(format!("Proofreading {round}: {summary}"));
        }
        let unchanged = checked == song;
        let (_, new_issues) = validate(&checked);
        if error_count(&new_issues) > error_count(&issues) {
            emit(Event::Log(format!("Proofreading {round} introduced errors; running auto-fix.")));
        }
        song = checked;
        revisions.push((format!("Proofreading {round}"), song.clone()));
        let (s, i) = fix_loop(env, &ctx, &images, song, &mut usage, &mut revisions, emit)?;
        song = s;
        issues = i;
        if unchanged {
            break;
        }
    }

    emit(Event::Status(format!("Done — {}", describe_issues(&issues))));
    // Show the reviewers' remarks (including remaining doubts) next to the music.
    let mut song = song;
    let review: Vec<&String> = notes.iter().filter(|n| !n.contains("No changes needed.") || n.len() > 60).collect();
    if !review.is_empty() {
        let joined = review.iter().map(|n| format!("• {n}")).collect::<Vec<_>>().join(
            "
",
        );
        song.transcription_notes = if song.transcription_notes.trim().is_empty() {
            joined
        } else {
            format!(
                "{}

{joined}",
                song.transcription_notes.trim()
            )
        };
    }
    Ok(Outcome { song, issues, usage, revisions, notes })
}

/// Apply a change the user described in words.
pub fn user_fix(env: &Env, input: &SongInput, request: &str, emit: &mut dyn FnMut(Event)) -> Result<Outcome> {
    let cfg = env.cfg;
    let current = input.current.clone().ok_or_else(|| anyhow!("Transcribe the song first."))?;
    let ctx = input.context(cfg);
    let images = input.images(cfg, cfg.accuracy.detail_in_review(), emit)?;
    let mut usage = Usage::default();
    let mut revisions = Vec::new();
    let system = prompts::transcription_system();
    emit(Event::Status("Asking Claude to make your change…".into()));
    let parts = prompts::user_fix(&ctx, &images, &current, request);
    let (song, summary) = call_for_song(env, "Requested change", &system, parts, PATCH_TOKENS, Some(&current), &mut usage, emit)?;
    if !summary.is_empty() {
        emit(Event::Log(format!("Change: {summary}")));
    }
    let short: String = request.chars().take(48).collect();
    revisions.push((format!("Change: {short}"), song.clone()));
    let (song, issues) = fix_loop(env, &ctx, &images, song, &mut usage, &mut revisions, emit)?;
    emit(Event::Status(format!("Done — {}", describe_issues(&issues))));
    Ok(Outcome { song, issues, usage, revisions, notes: vec![summary] })
}

/// An extra proofreading pass on demand.
pub fn proofread(env: &Env, input: &SongInput, emit: &mut dyn FnMut(Event)) -> Result<Outcome> {
    let cfg = env.cfg;
    let current = input.current.clone().ok_or_else(|| anyhow!("Transcribe the song first."))?;
    let ctx = input.context(cfg);
    let images = input.images(cfg, true, emit)?;
    let mut usage = Usage::default();
    let mut revisions = Vec::new();
    let system = prompts::transcription_system();
    let (_, issues) = validate(&current);
    let warnings: Vec<Issue> = issues.iter().filter(|i| i.severity == Severity::Warning).cloned().collect();
    emit(Event::Status("Proofreading against the pages…".into()));
    let parts = prompts::proofread(&ctx, &images, &current, 1, &warnings);
    let (song, summary) = call_for_song(env, "Proofreading", &system, parts, PATCH_TOKENS, Some(&current), &mut usage, emit)?;
    if !summary.is_empty() {
        emit(Event::Log(format!("Proofreading: {summary}")));
    }
    revisions.push(("Proofreading".to_string(), song.clone()));
    let (song, issues) = fix_loop(env, &ctx, &images, song, &mut usage, &mut revisions, emit)?;
    emit(Event::Status(format!("Done — {}", describe_issues(&issues))));
    Ok(Outcome { song, issues, usage, revisions, notes: vec![summary] })
}

// ---------------------------------------------------------------- song detection

#[derive(Deserialize, Clone, Debug, Default)]
#[serde(default)]
pub struct DetectedSongPart {
    pub number: String,
    pub title: String,
    pub first_line: String,
    pub position: String,
    pub starts_on_this_page: bool,
    pub ends_on_this_page: bool,
    pub has_music: bool,
}

#[derive(Deserialize, Clone, Debug, Default)]
#[serde(default)]
pub struct DetectedPage {
    pub page: usize,
    pub book_page: String,
    pub songs: Vec<DetectedSongPart>,
}

#[derive(Deserialize, Clone, Debug, Default)]
#[serde(default)]
struct DetectAnswer {
    pages: Vec<DetectedPage>,
}

/// A song assembled from detection results.
#[derive(Clone, Debug)]
pub struct DetectedSong {
    pub number: String,
    pub title: String,
    pub first_line: String,
    pub pages: Vec<SongPage>,
}

/// Detect songs across a sequence of loose pages (processed in small batches).
pub fn detect_songs(
    env: &Env,
    kind: CollectionKind,
    book: &BookInfo,
    images_dir: &std::path::Path,
    pages: &[SongPage],
    emit: &mut dyn FnMut(Event),
) -> Result<(Vec<DetectedSong>, Usage)> {
    const BATCH: usize = 6;
    let mut usage = Usage::default();
    let mut detected: Vec<DetectedPage> = vec![DetectedPage::default(); pages.len()];
    let mut start = 0;
    while start < pages.len() {
        let end = (start + BATCH).min(pages.len());
        emit(Event::Status(format!("Looking for songs on pages {}–{} of {}…", start + 1, end, pages.len())));
        let mut imgs: Vec<(String, ImageInput)> = Vec::new();
        for p in &pages[start..end] {
            let img = imaging::processed_page(images_dir, p)?;
            let label = if p.book_page.is_empty() {
                p.original_path.rsplit(['/', '\\']).next().unwrap_or("").to_string()
            } else {
                format!("printed page {}", p.book_page)
            };
            imgs.push((label, imaging::encode_jpeg(&imaging::fit(&img, 1400))?));
        }
        let parts = prompts::detect_songs(kind, book, &imgs, start + 1);
        let text = call(env, "Song detection", prompts::detect_system(), parts, 16_000, &mut usage, emit)?;
        let v = extract_json(&text)?;
        let ans: DetectAnswer = serde_json::from_value(v)?;
        for (k, dp) in ans.pages.into_iter().enumerate() {
            let idx = if dp.page > start && dp.page <= end { dp.page - 1 } else { start + k };
            if idx < detected.len() {
                detected[idx] = dp;
            }
        }
        start = end;
    }

    let songs = assemble_songs(&detected, pages);
    emit(Event::Status(format!("Found {} song(s)", songs.len())));
    Ok((songs, usage))
}

/// Turn per-page detection results into songs with their pages and
/// ignore-top/bottom settings, joining songs that continue onto the next page.
pub fn assemble_songs(detected: &[DetectedPage], pages: &[SongPage]) -> Vec<DetectedSong> {
    let mut songs: Vec<DetectedSong> = Vec::new();
    for (pi, dp) in detected.iter().enumerate().take(pages.len()) {
        let music: Vec<&DetectedSongPart> = dp.songs.iter().filter(|s| s.has_music).collect();
        let n_on_page = music.len();
        for (si, part) in music.iter().enumerate() {
            let mut page = pages[pi].clone();
            if page.book_page.is_empty() {
                page.book_page = dp.book_page.clone();
            }
            page.ignore_top = si > 0;
            page.ignore_bottom = si + 1 < n_on_page;
            if n_on_page > 2 && si > 0 && si + 1 < n_on_page {
                page.note = format!("Transcribe only the song \"{}\" ({}), which is in the middle of the page.", part.title, part.number);
            } else if n_on_page > 1 {
                page.note = format!("This song is \"{}\" ({}).", part.title, part.number);
            }
            // Only the first song on a page can continue from the previous page.
            let continues = si == 0
                && !part.starts_on_this_page
                && songs
                    .last()
                    .map(|s| same_song(s, part) && s.pages.last().map(|p| p.image != page.image).unwrap_or(true))
                    .unwrap_or(false);
            if continues {
                if let Some(last) = songs.last_mut() {
                    last.pages.push(page);
                    continue;
                }
            }
            songs.push(DetectedSong {
                number: part.number.clone(),
                title: part.title.clone(),
                first_line: part.first_line.clone(),
                pages: vec![page],
            });
        }
    }
    songs
}

fn same_song(s: &DetectedSong, part: &DetectedSongPart) -> bool {
    let norm = |x: &str| x.to_lowercase().chars().filter(|c| c.is_alphanumeric()).collect::<String>();
    // Titles decide when both are known (two tunes can share a hymn number).
    if !s.title.is_empty() && !part.title.is_empty() {
        return norm(&s.title) == norm(&part.title);
    }
    !s.number.is_empty() && norm(&s.number) == norm(&part.number)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn part(n: &str, t: &str, starts: bool) -> DetectedSongPart {
        DetectedSongPart {
            number: n.into(),
            title: t.into(),
            starts_on_this_page: starts,
            ends_on_this_page: true,
            has_music: true,
            ..Default::default()
        }
    }

    #[test]
    fn assembles_songs_across_pages() {
        let pages: Vec<SongPage> = (0..3).map(|i| SongPage { image: format!("p{i}.jpg"), ..Default::default() }).collect();
        let detected = vec![
            DetectedPage { page: 1, book_page: "25".into(), songs: vec![part("35", "Pleyel's Hymn", true), part("36", "Belmont", true)] },
            // 36 continues at the top of page 26; two tunes of 37 follow.
            DetectedPage {
                page: 2,
                book_page: "26".into(),
                songs: vec![part("36", "Belmont", false), part("37", "Westcott", true), part("37", "Malvern", true)],
            },
            DetectedPage { page: 3, book_page: "27".into(), songs: vec![part("37", "Malvern", false)] },
        ];
        let songs = assemble_songs(&detected, &pages);
        let summary: Vec<(String, usize)> = songs.iter().map(|s| (s.title.clone(), s.pages.len())).collect();
        assert_eq!(summary, vec![("Pleyel's Hymn".into(), 1), ("Belmont".into(), 2), ("Westcott".into(), 1), ("Malvern".into(), 2)]);
        // Belmont: bottom of page 25 (ignore top), top of page 26 (ignore bottom).
        let b = &songs[1];
        assert!(b.pages[0].ignore_top && !b.pages[0].ignore_bottom);
        assert!(!b.pages[1].ignore_top && b.pages[1].ignore_bottom);
        // Westcott is in the middle of page 26.
        assert!(songs[2].pages[0].ignore_top && songs[2].pages[0].ignore_bottom);
        assert!(songs[2].pages[0].note.contains("middle"));
    }
}

/// Read book details from title / copyright page photos.
pub fn read_book_info(env: &Env, paths: &[PathBuf], emit: &mut dyn FnMut(Event)) -> Result<(BookInfo, Usage)> {
    let mut usage = Usage::default();
    let mut imgs = Vec::new();
    for p in paths {
        let img = imaging::load(p)?;
        imgs.push(imaging::encode_jpeg(&imaging::fit(&img, 1600))?);
    }
    emit(Event::Status("Reading the book details…".into()));
    let text = call(env, "Book details", prompts::book_info_system(), prompts::book_info(&imgs), 4_000, &mut usage, emit)?;
    let info: BookInfo = serde_json::from_value(extract_json(&text)?)?;
    Ok((info, usage))
}

/// A tiny request to confirm the backend works.
pub fn test_connection(env: &Env, emit: &mut dyn FnMut(Event)) -> Result<String> {
    let mut usage = Usage::default();
    let parts = vec![Part::Text("Reply with exactly: READY".into())];
    let text =
        call(env, "Connection test", "You are a connection test. Follow the instruction exactly.".into(), parts, 200, &mut usage, emit)?;
    Ok(text.trim().to_string())
}
