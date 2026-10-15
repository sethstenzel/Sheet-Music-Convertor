//! The song editor: pages, transcription, review, details and export.

use super::widgets::{self, Tex};
use super::{App, Confirm, JobTag, SongTab, ToastKind, View};
use crate::config::{Accuracy, VoiceLayout};
use crate::export::ExportSettings;
use crate::jobs::JobKind;
use crate::model::notation::ParsedSong;
use crate::model::project::SongStatus;
use crate::model::score::{KeySig, Measure, Song, Stanza, Tempo, VoiceMeasure};
use crate::model::validate::{validate, Severity};
use crate::pipeline::SongInput;
use eframe::egui::{self, Color32, RichText};
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum ReviewMode {
    #[default]
    Photo,
    Notation,
}

pub struct SongUi {
    pub preview_pages: Vec<PathBuf>,
    pub preview_error: Option<String>,
    pub json_text: Option<String>,
    json_error: Option<String>,
    fix_request: String,
    selected: Option<usize>,
    crop_page: Option<usize>,
    crop_drag: Option<egui::Pos2>,
    crop_tmp: Option<[f32; 4]>,
    view_page: usize,
    preview_page: usize,
    zoom: f32,
    mode: ReviewMode,
    edit: Option<MeasureDraft>,
    details: Option<Song>,
    export: Option<ExportSettings>,
    parsed_cache: Option<(Song, ParsedSong)>,
}

impl Default for SongUi {
    fn default() -> Self {
        Self {
            preview_pages: Vec::new(),
            preview_error: None,
            json_text: None,
            json_error: None,
            fix_request: String::new(),
            selected: None,
            crop_page: None,
            crop_drag: None,
            crop_tmp: None,
            view_page: 0,
            preview_page: 0,
            zoom: 1.0,
            mode: ReviewMode::Photo,
            edit: None,
            details: None,
            export: None,
            parsed_cache: None,
        }
    }
}

impl SongUi {
    fn parsed(&mut self, song: &Song) -> &ParsedSong {
        let stale = self.parsed_cache.as_ref().map(|(s, _)| s != song).unwrap_or(true);
        if stale {
            let (p, _) = validate(song);
            self.parsed_cache = Some((song.clone(), p));
        }
        &self.parsed_cache.as_ref().unwrap().1
    }
}

/// Editable copy of one measure.
#[derive(Clone)]
struct MeasureDraft {
    index: usize,
    measure: Measure,
    /// voice id → (notes, lyrics as "1: text" lines)
    voices: BTreeMap<String, (String, String)>,
    ending: String,
}

impl MeasureDraft {
    fn new(index: usize, m: &Measure, song: &Song) -> Self {
        let mut voices = BTreeMap::new();
        for v in &song.voices {
            let vm = m.voices.get(&v.id).cloned().unwrap_or_default();
            let lyr = vm.lyrics.iter().map(|(k, t)| format!("{k}: {t}")).collect::<Vec<_>>().join("\n");
            voices.insert(v.id.clone(), (vm.notes, lyr));
        }
        Self { index, measure: m.clone(), voices, ending: m.ending.clone().unwrap_or_default() }
    }

    fn build(&self) -> Measure {
        let mut m = self.measure.clone();
        m.ending = if self.ending.trim().is_empty() { None } else { Some(self.ending.trim().to_string()) };
        m.voices.clear();
        for (id, (notes, lyr)) in &self.voices {
            if notes.trim().is_empty() {
                continue;
            }
            let mut lyrics = BTreeMap::new();
            for line in lyr.lines() {
                if let Some((k, t)) = line.split_once(':') {
                    if let Ok(k) = k.trim().parse::<u32>() {
                        if !t.trim().is_empty() {
                            lyrics.insert(k, t.trim().to_string());
                        }
                    }
                }
            }
            m.voices.insert(id.clone(), VoiceMeasure { notes: notes.trim().to_string(), lyrics });
        }
        m
    }
}

// ------------------------------------------------------------------ actions

pub fn song_input(app: &App, id: &str) -> Option<SongInput> {
    let p = app.project.as_ref()?;
    let e = p.song(id)?;
    Some(SongInput {
        kind: p.kind,
        book: p.book.clone(),
        images_dir: p.images_dir(),
        pages: e.pages.clone(),
        title_hint: e.title_hint.clone(),
        number_hint: e.number_hint.clone(),
        instructions: e.instructions.clone(),
        current: e.song.clone(),
    })
}

pub fn start_transcription(app: &mut App, id: &str) {
    if !app.backend_ready() {
        app.toast(ToastKind::Error, "Sign in to Claude first (Account).");
        app.view = View::Account;
        return;
    }
    let Some(input) = song_input(app, id) else { return };
    if input.pages.is_empty() {
        app.toast(ToastKind::Error, "Add the song's page images first.");
        return;
    }
    if app.jobs.is_busy(id) {
        return;
    }
    let title = app.entry(id).map(|e| e.display_title()).unwrap_or_default();
    app.submit(JobTag::Transcribe, format!("Transcribe \"{title}\""), JobKind::Transcribe { song_id: id.to_string(), input });
}

pub fn add_pages(app: &mut App, id: &str, paths: &[PathBuf]) {
    let Some(p) = app.project.as_ref() else { return };
    let mut pages = Vec::new();
    let mut errs = Vec::new();
    for path in paths {
        match p.import_image(path) {
            Ok(pg) => pages.push(pg),
            Err(e) => errs.push(format!("{e:#}")),
        }
    }
    let n = pages.len();
    if let Some(e) = app.entry_mut(id) {
        // Guess the printed page number from the previous page.
        for mut pg in pages {
            if let Some(prev) = e.pages.last() {
                if let Ok(n) = prev.book_page.trim().parse::<u32>() {
                    pg.book_page = (n + 1).to_string();
                }
            }
            e.pages.push(pg);
        }
    }
    if n > 0 {
        app.mark_dirty();
        app.toast(ToastKind::Success, format!("Added {n} page(s)"));
    }
    for e in errs {
        app.toast(ToastKind::Error, e);
    }
}

fn commit_song(app: &mut App, id: &str, label: &str, song: Song) {
    let (_, issues) = validate(&song);
    if let Some(e) = app.entry_mut(id) {
        e.push_revision(label, song);
        e.issues = issues;
        if e.status == SongStatus::Reviewed {
            e.status = SongStatus::NeedsReview;
        }
    }
    if let Some(ui) = app.song_ui.get_mut(id) {
        ui.preview_pages.clear();
        ui.json_text = None;
    }
    app.mark_dirty();
}

fn temp_dir(sub: &str) -> PathBuf {
    let d = crate::config::data_dir().join("temp").join(sub);
    let _ = std::fs::create_dir_all(&d);
    d
}

fn open_in_musescore(app: &mut App, id: &str) {
    let Some(p) = app.project.as_ref() else { return };
    let Some(e) = p.song(id) else { return };
    let Some(song) = &e.song else { return };
    let settings = app.song_ui.get(id).and_then(|u| u.export.clone()).unwrap_or_else(|| app.cfg.export.clone());
    let parsed = crate::export::prepare(song, settings.transpose_semitones);
    let xml = crate::export::musicxml::write_musicxml(&parsed, &settings.xml, &p.source_info(e));
    let dir = temp_dir("open");
    let path = dir.join(format!("{}.musicxml", crate::export::song_file_stem(p, e, &settings)));
    let res = std::fs::write(&path, xml)
        .map_err(anyhow::Error::from)
        .and_then(|_| crate::export::musescore::open_in_musescore(app.musescore.as_deref(), &path));
    if let Err(err) = res {
        app.toast(ToastKind::Error, format!("Could not open the score: {err:#}"));
    }
}

fn play_midi(app: &mut App, id: &str) {
    let Some(e) = app.entry(id) else { return };
    let Some(song) = &e.song else { return };
    let settings = app.song_ui.get(id).and_then(|u| u.export.clone()).unwrap_or_else(|| app.cfg.export.clone());
    let parsed = crate::export::prepare(song, settings.transpose_semitones);
    let bytes = crate::export::midi::write_midi(&parsed, &settings.midi_opts);
    let path = temp_dir("play").join(format!("{}.mid", crate::export::sanitize_filename(&song.display_title())));
    let res =
        std::fs::write(&path, bytes).map_err(anyhow::Error::from).and_then(|_| crate::export::musescore::open_with_default_app(&path));
    if let Err(err) = res {
        app.toast(ToastKind::Error, format!("Could not play MIDI: {err:#}"));
    }
}

// ------------------------------------------------------------------ view

pub fn song_view(app: &mut App, ui: &mut egui::Ui, id: &str) {
    app.song_ui.entry(id.to_string()).or_default();
    let (title, number, status, has_song, n_pages, err_count, warn_count) = {
        let e = app.entry(id).unwrap();
        let errs = e.issues.iter().filter(|i| i.severity == Severity::Error).count();
        (e.display_title(), e.display_number(), e.status, e.song.is_some(), e.pages.len(), errs, e.issues.len() - errs)
    };
    let busy = app.jobs.is_busy(id);

    ui.horizontal(|ui| {
        let heading = if number.is_empty() { title.clone() } else { format!("{number}. {title}") };
        ui.heading(heading);
        ui.label(RichText::new(status.label()).weak());
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if busy {
                if ui.button("■ Cancel").clicked() {
                    app.jobs.cancel_song(id);
                }
                ui.spinner();
            } else {
                let label = if has_song { "↻ Transcribe again" } else { "▶ Transcribe" };
                if ui
                    .add_enabled(n_pages > 0, egui::Button::new(RichText::new(label).strong()))
                    .on_disabled_hover_text("Add page images first")
                    .clicked()
                {
                    if has_song {
                        app.confirm = Some(Confirm::RetranscribeSong(id.to_string()));
                    } else {
                        start_transcription(app, id);
                        app.song_tab = SongTab::Transcribe;
                    }
                }
            }
        });
    });
    ui.horizontal(|ui| {
        for (tab, label) in [
            (SongTab::Pages, format!("📄 Pages ({n_pages})")),
            (SongTab::Transcribe, "⚙ Transcribe".to_string()),
            (
                SongTab::Review,
                if err_count + warn_count > 0 {
                    format!("🔍 Review ({err_count} errors, {warn_count} warnings)")
                } else {
                    "🔍 Review".to_string()
                },
            ),
            (SongTab::Details, "📝 Details".to_string()),
            (SongTab::Export, "💾 Export".to_string()),
        ] {
            if ui.selectable_label(app.song_tab == tab, label).clicked() {
                app.song_tab = tab;
            }
        }
    });
    ui.separator();
    match app.song_tab {
        SongTab::Pages => pages_tab(app, ui, id),
        SongTab::Transcribe => transcribe_tab(app, ui, id),
        SongTab::Review => review_tab(app, ui, id),
        SongTab::Details => details_tab(app, ui, id),
        SongTab::Export => export_tab(app, ui, id),
    }
    crop_window(app, ui.ctx(), id);
}

// ------------------------------------------------------------------ pages

fn pages_tab(app: &mut App, ui: &mut egui::Ui, id: &str) {
    let ctx = ui.ctx().clone();
    let images_dir = app.project.as_ref().unwrap().images_dir();
    let is_book = app.project.as_ref().unwrap().kind.is_book();
    let mut entry = app.entry(id).unwrap().clone();
    let before = entry.clone();
    let mut action: Option<(usize, &'static str)> = None;

    egui::ScrollArea::vertical().id_salt("pages_tab").show(ui, |ui| {
        egui::Frame::group(ui.style()).inner_margin(10.0).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new("About this song (optional — helps Claude find the right song)").strong());
            egui::Grid::new("hints").num_columns(2).spacing([10.0, 4.0]).show(ui, |ui| {
                ui.label("Title");
                ui.add(egui::TextEdit::singleline(&mut entry.title_hint).hint_text("as printed").desired_width(360.0));
                ui.end_row();
                if is_book {
                    ui.label("Number in book");
                    ui.add(egui::TextEdit::singleline(&mut entry.number_hint).hint_text("e.g. 245").desired_width(120.0));
                    ui.end_row();
                }
                ui.label("Notes for Claude");
                ui.add(
                    egui::TextEdit::multiline(&mut entry.instructions)
                        .hint_text("e.g. \"The refrain is printed once and sung after every verse\" or \"Ignore the descant\"")
                        .desired_rows(2)
                        .desired_width(480.0),
                );
                ui.end_row();
            });
        });
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            let label = if entry.pages.is_empty() { "➕ Add first page…" } else { "➕ Add next page…" };
            if ui.button(RichText::new(label).strong()).clicked() {
                action = Some((usize::MAX, "add"));
            }
            ui.label(RichText::new("Add the song's pages one at a time, in order. You can also drop image files here.").weak());
        });
        ui.add_space(6.0);
        if entry.pages.is_empty() {
            ui.label(RichText::new("No pages yet.").weak());
        }
        let n = entry.pages.len();
        for (i, page) in entry.pages.iter_mut().enumerate() {
            egui::Frame::group(ui.style()).inner_margin(8.0).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal_top(|ui| {
                    let path = images_dir.join(&page.image);
                    match app.tex.get(&ctx, &path, page.rotation, 900) {
                        Tex::Ready(t) => {
                            let (_, rect) = widgets::fitted_image(ui, t, egui::vec2(260.0, 340.0), egui::Sense::hover());
                            widgets::page_overlay(ui.painter(), rect, page.crop, page.ignore_top, page.ignore_bottom);
                        }
                        Tex::Loading => {
                            ui.add_sized([260.0, 340.0], egui::Spinner::new());
                        }
                        Tex::Failed(e) => {
                            ui.add_sized([260.0, 80.0], egui::Label::new(RichText::new(format!("Can't show image: {e}")).small()));
                        }
                    }
                    ui.vertical(|ui| {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(format!("Page {} of {n}", i + 1)).strong());
                            ui.label("Printed page no.");
                            ui.add(egui::TextEdit::singleline(&mut page.book_page).desired_width(60.0));
                        });
                        ui.checkbox(&mut page.ignore_top, "Ignore song at the top of this page")
                            .on_hover_text("The top of this page holds the end of a different song.");
                        ui.checkbox(&mut page.ignore_bottom, "Ignore song at the bottom of this page")
                            .on_hover_text("The bottom of this page holds the start of a different song.");
                        ui.checkbox(&mut page.enhance, "Improve contrast (remove shadows, grayscale)");
                        ui.horizontal(|ui| {
                            if ui.button("⟲").on_hover_text("Rotate left").clicked() {
                                page.rotation = (page.rotation + 270) % 360;
                                page.crop = None;
                            }
                            if ui.button("⟳").on_hover_text("Rotate right").clicked() {
                                page.rotation = (page.rotation + 90) % 360;
                                page.crop = None;
                            }
                            if ui.button("✂ Crop…").clicked() {
                                action = Some((i, "crop"));
                            }
                            if page.crop.is_some() && ui.button("Remove crop").clicked() {
                                page.crop = None;
                            }
                        });
                        ui.label("Note for Claude about this page");
                        ui.add(
                            egui::TextEdit::singleline(&mut page.note)
                                .hint_text("e.g. only the second song on this page (#246)")
                                .desired_width(380.0),
                        );
                        ui.horizontal(|ui| {
                            if ui.add_enabled(i > 0, egui::Button::new("▲ Move up")).clicked() {
                                action = Some((i, "up"));
                            }
                            if ui.add_enabled(i + 1 < n, egui::Button::new("▼ Move down")).clicked() {
                                action = Some((i, "down"));
                            }
                            if ui.button("Replace image…").clicked() {
                                action = Some((i, "replace"));
                            }
                            if ui.button("🗑 Remove").clicked() {
                                action = Some((i, "remove"));
                            }
                        });
                        if !page.original_path.is_empty() {
                            ui.label(RichText::new(&page.original_path).small().weak());
                        }
                    });
                });
            });
            ui.add_space(4.0);
        }
    });

    if let Some((i, what)) = action {
        match what {
            "add" => {
                if let Some(path) = rfd::FileDialog::new()
                    .add_filter("Images", crate::imaging::SUPPORTED_EXTENSIONS)
                    .set_title(format!("Page {} of the song", entry.pages.len() + 1))
                    .pick_file()
                {
                    // Save hint edits first, then import.
                    if entry != before {
                        if let Some(e) = app.entry_mut(id) {
                            *e = entry.clone();
                        }
                    }
                    add_pages(app, id, &[path]);
                    return;
                }
            }
            "up" => entry.pages.swap(i, i - 1),
            "down" => entry.pages.swap(i, i + 1),
            "remove" => {
                entry.pages.remove(i);
            }
            "replace" => {
                if let Some(path) = rfd::FileDialog::new().add_filter("Images", crate::imaging::SUPPORTED_EXTENSIONS).pick_file() {
                    match app.project.as_ref().unwrap().import_image(&path) {
                        Ok(np) => {
                            entry.pages[i].image = np.image;
                            entry.pages[i].original_path = np.original_path;
                            entry.pages[i].crop = None;
                            entry.pages[i].rotation = 0;
                        }
                        Err(e) => app.toast(ToastKind::Error, format!("{e:#}")),
                    }
                }
            }
            "crop" => {
                let ui_state = app.song_ui.get_mut(id).unwrap();
                ui_state.crop_page = Some(i);
                ui_state.crop_tmp = entry.pages[i].crop;
            }
            _ => {}
        }
    }
    if entry != before {
        if let Some(e) = app.entry_mut(id) {
            *e = entry;
        }
        app.mark_dirty();
    }
}

fn crop_window(app: &mut App, ctx: &egui::Context, id: &str) {
    let Some(pi) = app.song_ui.get(id).and_then(|u| u.crop_page) else { return };
    let Some(page) = app.entry(id).and_then(|e| e.pages.get(pi).cloned()) else {
        app.song_ui.get_mut(id).unwrap().crop_page = None;
        return;
    };
    let path = app.project.as_ref().unwrap().image_path(&page.image);
    let mut open = true;
    let mut apply = None;
    egui::Window::new(format!("Crop page {}", pi + 1)).open(&mut open).default_size([760.0, 860.0]).show(ctx, |ui| {
        ui.label("Drag a rectangle around the part of the page that belongs to this song.");
        let max = egui::vec2(ui.available_width().min(1000.0), (ui.ctx().screen_rect().height() - 220.0).max(300.0));
        let state = app.song_ui.get_mut(id).unwrap();
        let (mut tmp, mut drag) = (state.crop_tmp, state.crop_drag);
        match app.tex.get(ctx, &path, page.rotation, 1600) {
            Tex::Ready(t) => {
                widgets::crop_editor(ui, t, max, &mut tmp, &mut drag);
            }
            Tex::Loading => {
                ui.spinner();
            }
            Tex::Failed(e) => {
                ui.label(e);
            }
        }
        let state = app.song_ui.get_mut(id).unwrap();
        state.crop_tmp = tmp;
        state.crop_drag = drag;
        ui.horizontal(|ui| {
            if ui.button(RichText::new("Apply").strong()).clicked() {
                apply = Some(tmp);
            }
            if ui.button("No crop").clicked() {
                apply = Some(None);
            }
        });
    });
    if let Some(c) = apply {
        if let Some(e) = app.entry_mut(id) {
            if let Some(p) = e.pages.get_mut(pi) {
                p.crop = c;
            }
        }
        app.mark_dirty();
        open = false;
    }
    if !open {
        app.song_ui.get_mut(id).unwrap().crop_page = None;
    }
}

// ------------------------------------------------------------------ transcribe

fn transcribe_tab(app: &mut App, ui: &mut egui::Ui, id: &str) {
    let busy = app.jobs.is_busy(id);
    let running = app
        .jobs
        .running
        .values()
        .find(|r| r.song_id.as_deref() == Some(id))
        .map(|r| (r.status.clone(), r.streamed, r.started.elapsed().as_secs()));
    egui::Frame::group(ui.style()).inner_margin(10.0).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.label(RichText::new("Settings").strong());
        let mut changed = false;
        egui::Grid::new("tx_settings").num_columns(2).spacing([10.0, 4.0]).show(ui, |ui| {
            ui.label("Accuracy");
            egui::ComboBox::from_id_salt("tx_acc").selected_text(app.cfg.accuracy.label()).show_ui(ui, |ui| {
                for a in Accuracy::ALL {
                    changed |= ui.selectable_value(&mut app.cfg.accuracy, a, a.label()).changed();
                }
            });
            ui.end_row();
            ui.label("Voices");
            egui::ComboBox::from_id_salt("tx_layout").selected_text(app.cfg.voice_layout.label()).show_ui(ui, |ui| {
                for l in VoiceLayout::ALL {
                    changed |= ui.selectable_value(&mut app.cfg.voice_layout, l, l.label()).changed();
                }
            });
            ui.end_row();
            ui.label("Model");
            egui::ComboBox::from_id_salt("tx_model")
                .selected_text(crate::claude::MODEL_CHOICES.iter().find(|m| m.0 == app.cfg.model).map(|m| m.1).unwrap_or(&app.cfg.model))
                .show_ui(ui, |ui| {
                    for (v, l) in crate::claude::MODEL_CHOICES {
                        changed |= ui.selectable_value(&mut app.cfg.model, v.to_string(), *l).changed();
                    }
                });
            ui.end_row();
        });
        if changed {
            app.save_config();
        }
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            let has_pages = app.entry(id).map(|e| !e.pages.is_empty()).unwrap_or(false);
            let has_song = app.entry(id).map(|e| e.song.is_some()).unwrap_or(false);
            if busy {
                if ui.button("■ Cancel").clicked() {
                    app.jobs.cancel_song(id);
                }
            } else if ui
                .add_enabled(
                    has_pages,
                    egui::Button::new(RichText::new(if has_song { "↻ Transcribe again" } else { "▶ Start transcription" }).strong()),
                )
                .clicked()
            {
                if has_song {
                    app.confirm = Some(Confirm::RetranscribeSong(id.to_string()));
                } else {
                    start_transcription(app, id);
                }
            }
            if !app.backend_ready() {
                ui.label(RichText::new("Not signed in to Claude").color(Color32::from_rgb(220, 80, 80)));
                if ui.link("Account…").clicked() {
                    app.view = View::Account;
                }
            }
        });
        if let Some((status, streamed, secs)) = running {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(RichText::new(status).strong());
                ui.label(format!("{}:{:02}", secs / 60, secs % 60));
                if streamed > 0 {
                    ui.label(format!("· {} KB received", streamed / 1024));
                }
            });
            ui.label(
                RichText::new("A full hymn usually takes a few minutes per pass. You can keep working on other songs meanwhile.").weak(),
            );
        } else if busy {
            ui.label("Waiting in the queue…");
        }
    });
    let (log, last_error, usage) = {
        let e = app.entry(id).unwrap();
        (e.log.clone(), e.last_error.clone(), e.usage.clone())
    };
    if !last_error.is_empty() {
        ui.add_space(6.0);
        egui::Frame::group(ui.style()).stroke(egui::Stroke::new(1.0_f32, Color32::from_rgb(200, 60, 60))).inner_margin(8.0).show(
            ui,
            |ui| {
                ui.set_width(ui.available_width());
                ui.label(RichText::new("The last run failed").strong().color(Color32::from_rgb(220, 80, 80)));
                ui.label(&last_error);
            },
        );
    }
    if usage.requests > 0 {
        ui.label(
            RichText::new(format!("This song so far: {} Claude requests, {}k tokens out", usage.requests, usage.output_tokens / 1000))
                .weak(),
        );
    }
    ui.add_space(6.0);
    ui.label(RichText::new("Activity").strong());
    egui::ScrollArea::vertical().id_salt("tx_log").stick_to_bottom(true).max_height(ui.available_height()).show(ui, |ui| {
        if log.is_empty() {
            ui.label(RichText::new("Nothing yet.").weak());
        }
        for l in &log {
            ui.label(RichText::new(l).monospace().small());
        }
    });
}

// ------------------------------------------------------------------ review

fn review_tab(app: &mut App, ui: &mut egui::Ui, id: &str) {
    let Some(song) = app.entry(id).and_then(|e| e.song.clone()) else {
        ui.label("This song has not been transcribed yet. Add its pages and click Transcribe.");
        return;
    };
    let busy = app.jobs.is_busy(id);
    ui.horizontal_wrapped(|ui| {
        let reviewed = app.entry(id).map(|e| e.status == SongStatus::Reviewed).unwrap_or(false);
        if !reviewed {
            if ui.button("✔ Mark as reviewed").clicked() {
                if let Some(e) = app.entry_mut(id) {
                    e.status = SongStatus::Reviewed;
                }
                app.mark_dirty();
            }
        } else {
            ui.label(RichText::new("✔ Reviewed").color(Color32::from_rgb(60, 190, 100)));
        }
        if ui
            .add_enabled(!busy, egui::Button::new("🔍 Proofread again"))
            .on_hover_text("Claude compares the whole transcription with the photos once more")
            .clicked()
        {
            if let Some(input) = song_input(app, id) {
                if app.backend_ready() {
                    app.submit(
                        JobTag::Proofread,
                        format!("Proofread \"{}\"", song.display_title()),
                        JobKind::Proofread { song_id: id.to_string(), input },
                    );
                } else {
                    app.toast(ToastKind::Error, "Sign in to Claude first (Account).");
                }
            }
        }
        if ui.button("🎹 Play MIDI").clicked() {
            play_midi(app, id);
        }
        if ui.button("🎼 Open in MuseScore").clicked() {
            open_in_musescore(app, id);
        }
    });
    ui.separator();

    let avail = ui.available_size();
    let left_w = (avail.x * 0.48).max(320.0);
    ui.horizontal_top(|ui| {
        ui.allocate_ui_with_layout(egui::vec2(left_w, avail.y), egui::Layout::top_down(egui::Align::Min), |ui| {
            image_panel(app, ui, id, &song);
        });
        ui.separator();
        ui.allocate_ui_with_layout(egui::vec2(ui.available_width(), avail.y), egui::Layout::top_down(egui::Align::Min), |ui| {
            egui::ScrollArea::vertical().id_salt("review_right").show(ui, |ui| {
                transcription_panel(app, ui, id, &song, busy);
            });
        });
    });
}

fn image_panel(app: &mut App, ui: &mut egui::Ui, id: &str, song: &Song) {
    let ctx = ui.ctx().clone();
    let pages = app.entry(id).map(|e| e.pages.clone()).unwrap_or_default();
    let images_dir = app.project.as_ref().unwrap().images_dir();
    let has_ms = app.musescore.is_some();
    let state = app.song_ui.get_mut(id).unwrap();
    ui.horizontal(|ui| {
        ui.selectable_value(&mut state.mode, ReviewMode::Photo, "📷 Photo");
        ui.selectable_value(&mut state.mode, ReviewMode::Notation, "🎼 Notation");
        ui.separator();
        ui.label("Zoom");
        ui.add(egui::Slider::new(&mut state.zoom, 0.5..=4.0).show_value(false));
    });
    match state.mode {
        ReviewMode::Photo => {
            if pages.is_empty() {
                ui.label("No pages.");
                return;
            }
            ui.horizontal(|ui| {
                for i in 0..pages.len() {
                    ui.selectable_value(&mut state.view_page, i, format!("Page {}", i + 1));
                }
            });
            state.view_page = state.view_page.min(pages.len() - 1);
            let page = &pages[state.view_page];
            let zoom = state.zoom;
            let path = images_dir.join(&page.image);
            egui::ScrollArea::both().id_salt("photo_scroll").show(ui, |ui| match app.tex.get(&ctx, &path, page.rotation, 2400) {
                Tex::Ready(t) => {
                    let w = ui.available_width() * zoom;
                    let (_, rect) = widgets::fitted_image(ui, t, egui::vec2(w, w * 4.0), egui::Sense::hover());
                    widgets::page_overlay(ui.painter(), rect, page.crop, page.ignore_top, page.ignore_bottom);
                }
                Tex::Loading => {
                    ui.spinner();
                }
                Tex::Failed(e) => {
                    ui.label(e);
                }
            });
        }
        ReviewMode::Notation => {
            if !has_ms {
                ui.label("Install MuseScore Studio to see the engraved notation here. Meanwhile, use the piano roll on the right or 'Open in MuseScore'.");
                ui.hyperlink_to("Download MuseScore Studio", "https://musescore.org/download");
                return;
            }
            let rendering = app.job_tags.iter().any(|(jid, t)| {
                *t == JobTag::Preview && app.jobs.running.get(jid).map(|r| r.song_id.as_deref() == Some(id)).unwrap_or(false)
            }) || app.jobs.queued().any(|j| matches!(&j.kind, JobKind::Preview { song_id, .. } if song_id == id));
            let state = app.song_ui.get_mut(id).unwrap();
            let pages = state.preview_pages.clone();
            let mut render = false;
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(!rendering, egui::Button::new(if pages.is_empty() { "Render notation" } else { "Render again" }))
                    .clicked()
                {
                    render = true;
                }
                if rendering {
                    ui.spinner();
                }
                for i in 0..pages.len() {
                    ui.selectable_value(&mut state.preview_page, i, format!("{}", i + 1));
                }
            });
            if let Some(e) = &state.preview_error {
                ui.colored_label(Color32::from_rgb(220, 80, 80), e);
            }
            if render {
                state.preview_error = None;
                let p = app.project.as_ref().unwrap();
                let e = p.song(id).unwrap();
                let settings = state.export.clone().unwrap_or_else(|| app.cfg.export.clone());
                let kind = JobKind::Preview {
                    song_id: id.to_string(),
                    song: Box::new(song.clone()),
                    source: p.source_info(e),
                    xml: settings.xml.clone(),
                    transpose: settings.transpose_semitones,
                    musescore: app.musescore.clone().unwrap(),
                    dir: temp_dir(&format!("preview-{id}")),
                };
                app.submit(JobTag::Preview, "Render notation", kind);
            }
            if pages.is_empty() && !rendering {
                ui.label(
                    RichText::new("Click 'Render notation' to engrave the transcription with MuseScore and compare it with the photo.")
                        .weak(),
                );
            }
            if let Some(path) = pages.get(app.song_ui[id].preview_page.min(pages.len().saturating_sub(1))) {
                let zoom = app.song_ui[id].zoom;
                egui::ScrollArea::both().id_salt("notation_scroll").show(ui, |ui| match app.tex.get(&ctx, path, 0, 2400) {
                    Tex::Ready(t) => {
                        let w = ui.available_width() * zoom;
                        let (r, _) = widgets::fitted_image(ui, t, egui::vec2(w, w * 4.0), egui::Sense::hover());
                        let _ = r;
                    }
                    Tex::Loading => {
                        ui.spinner();
                    }
                    Tex::Failed(e) => {
                        ui.label(e);
                    }
                });
            }
        }
    }
}

fn transcription_panel(app: &mut App, ui: &mut egui::Ui, id: &str, song: &Song, busy: bool) {
    let issues = app.entry(id).map(|e| e.issues.clone()).unwrap_or_default();
    let parsed = app.song_ui.get_mut(id).unwrap().parsed(song).clone();

    // Summary
    egui::Grid::new("summary").num_columns(4).spacing([12.0, 2.0]).show(ui, |ui| {
        ui.label(RichText::new("Title").weak());
        ui.label(RichText::new(song.display_title()).strong());
        ui.label(RichText::new("Number").weak());
        ui.label(&song.meta.number);
        ui.end_row();
        ui.label(RichText::new("Key").weak());
        ui.label(song.key.name());
        ui.label(RichText::new("Time").weak());
        ui.label(&song.time);
        ui.end_row();
        ui.label(RichText::new("Measures").weak());
        ui.label(song.measures.len().to_string());
        ui.label(RichText::new("Verses").weak());
        ui.label(format!(
            "{}{}",
            song.verse_count,
            if song.extra_stanzas.is_empty() { String::new() } else { format!(" + {} below", song.extra_stanzas.len()) }
        ));
        ui.end_row();
        ui.label(RichText::new("Tune").weak());
        ui.label(format!("{} {}", song.meta.tune_name, song.meta.meter));
        ui.label(RichText::new("Voices").weak());
        ui.label(song.voices.iter().map(|v| v.id.clone()).collect::<Vec<_>>().join(" "));
        ui.end_row();
    });
    if !song.transcription_notes.trim().is_empty() {
        ui.add_space(4.0);
        egui::Frame::group(ui.style()).inner_margin(6.0).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new("Claude's notes").strong());
            ui.label(&song.transcription_notes);
        });
    }
    ui.add_space(4.0);

    // Issues
    let errs = issues.iter().filter(|i| i.severity == Severity::Error).count();
    egui::CollapsingHeader::new(format!("Checks: {errs} error(s), {} warning(s)", issues.len() - errs))
        .id_salt("issues")
        .default_open(!issues.is_empty() && issues.len() < 30)
        .show(ui, |ui| {
            if issues.is_empty() {
                ui.label(RichText::new("✔ No problems found by the automatic checks.").color(Color32::from_rgb(60, 190, 100)));
            }
            for (k, issue) in issues.iter().enumerate().take(200) {
                let color =
                    if issue.severity == Severity::Error { Color32::from_rgb(230, 90, 90) } else { Color32::from_rgb(220, 170, 60) };
                let number = issue.measure.and_then(|m| parsed.measures.get(m)).map(|m| format!("m. {}", m.number)).unwrap_or_default();
                let text = format!("{} {} {}", number, issue.voice.clone().unwrap_or_default(), issue.message);
                if ui
                    .add(egui::Label::new(RichText::new(text).color(color)).sense(egui::Sense::click()))
                    .on_hover_text("Click to edit this measure")
                    .clicked()
                {
                    if let Some(m) = issue.measure {
                        select_measure(app, id, song, m);
                    }
                }
                let _ = k;
            }
        });

    // Piano roll
    ui.add_space(4.0);
    ui.label(RichText::new("Piano roll (click a measure to edit it)").strong());
    widgets::voice_legend(ui, &parsed);
    let selected = app.song_ui[id].selected;
    if let Some(m) = widgets::piano_roll(ui, &parsed, selected, 170.0) {
        select_measure(app, id, song, m);
    }

    // Measure editor
    measure_editor(app, ui, id, song, &parsed);

    // Ask Claude
    ui.add_space(6.0);
    egui::Frame::group(ui.style()).inner_margin(8.0).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.label(RichText::new("Ask Claude to correct something").strong());
        let state = app.song_ui.get_mut(id).unwrap();
        ui.add(
            egui::TextEdit::multiline(&mut state.fix_request)
                .hint_text("e.g. \"In measure 6 the alto's second note is B flat\", \"Verse 3 is missing\", \"The title should be …\"")
                .desired_rows(3)
                .desired_width(f32::INFINITY),
        );
        let req = state.fix_request.trim().to_string();
        ui.horizontal(|ui| {
            if ui.add_enabled(!busy && !req.is_empty(), egui::Button::new("Send to Claude")).clicked() {
                if !app.backend_ready() {
                    app.toast(ToastKind::Error, "Sign in to Claude first (Account).");
                } else if let Some(input) = song_input(app, id) {
                    app.submit(JobTag::Fix, "Requested change", JobKind::UserFix { song_id: id.to_string(), input, request: req.clone() });
                    app.song_ui.get_mut(id).unwrap().fix_request.clear();
                }
            }
            if busy {
                ui.spinner();
                ui.label("Claude is working on this song…");
            }
        });
    });

    // Measures list
    ui.add_space(4.0);
    egui::CollapsingHeader::new("All measures").id_salt("all_measures").show(ui, |ui| {
        egui::Grid::new("measures_grid").striped(true).num_columns(2).spacing([8.0, 2.0]).show(ui, |ui| {
            for m in &parsed.measures {
                let label = format!("{}{}", m.number, if m.implicit && m.index > 0 { "′" } else { "" });
                if ui.selectable_label(app.song_ui[id].selected == Some(m.index), label).clicked() {
                    select_measure(app, id, song, m.index);
                }
                ui.vertical(|ui| {
                    for v in &m.voices {
                        let toks: Vec<String> = v.events.iter().map(|e| e.token()).collect();
                        ui.label(RichText::new(format!("{:>2} {}", v.id, toks.join(" "))).monospace().small());
                    }
                    if let Some(vm) = song.measures.get(m.index) {
                        for (vid, vmeas) in &vm.voices {
                            for (k, t) in &vmeas.lyrics {
                                ui.label(RichText::new(format!("   {vid}{k}: {t}")).small().weak());
                            }
                        }
                    }
                });
                ui.end_row();
            }
        });
    });

    // History
    egui::CollapsingHeader::new("History").id_salt("history").show(ui, |ui| {
        let history = app.entry(id).map(|e| e.history.clone()).unwrap_or_default();
        let mut restore = None;
        for (i, r) in history.iter().enumerate().rev() {
            ui.horizontal(|ui| {
                let when = chrono::DateTime::parse_from_rfc3339(&r.at).map(|d| d.format("%b %d %H:%M").to_string()).unwrap_or_default();
                let current = Some(&r.song) == app.entry(id).and_then(|e| e.song.as_ref());
                ui.label(format!("{when}  {}", r.label));
                if current {
                    ui.label(RichText::new("(current)").weak());
                } else if ui.small_button("Restore").clicked() {
                    restore = Some(i);
                }
            });
        }
        if let Some(i) = restore {
            let s = history[i].song.clone();
            commit_song(app, id, &format!("Restored: {}", history[i].label), s);
        }
    });

    // JSON
    egui::CollapsingHeader::new("Advanced: edit the transcription as JSON").id_salt("json").show(ui, |ui| {
        let state = app.song_ui.get_mut(id).unwrap();
        if state.json_text.is_none() {
            state.json_text = Some(serde_json::to_string_pretty(song).unwrap_or_default());
        }
        ui.label(RichText::new("The note format is described in the app's README. Changes are checked when you apply them.").weak());
        egui::ScrollArea::vertical().id_salt("json_scroll").max_height(420.0).show(ui, |ui| {
            ui.add(
                egui::TextEdit::multiline(state.json_text.as_mut().unwrap()).code_editor().desired_width(f32::INFINITY).desired_rows(20),
            );
        });
        if let Some(e) = &state.json_error {
            ui.colored_label(Color32::from_rgb(220, 80, 80), e);
        }
        let mut apply = None;
        ui.horizontal(|ui| {
            if ui.button("Apply JSON").clicked() {
                match serde_json::from_str::<Song>(state.json_text.as_ref().unwrap()) {
                    Ok(s) => apply = Some(s),
                    Err(e) => state.json_error = Some(format!("Invalid JSON: {e}")),
                }
            }
            if ui.button("Reset").clicked() {
                state.json_text = None;
                state.json_error = None;
            }
            if ui.button("Copy").clicked() {
                ui.ctx().copy_text(state.json_text.clone().unwrap_or_default());
            }
        });
        if let Some(s) = apply {
            app.song_ui.get_mut(id).unwrap().json_error = None;
            commit_song(app, id, "Edited JSON", s);
        }
    });
}

fn select_measure(app: &mut App, id: &str, song: &Song, m: usize) {
    let state = app.song_ui.get_mut(id).unwrap();
    state.selected = Some(m);
    state.edit = song.measures.get(m).map(|meas| MeasureDraft::new(m, meas, song));
}

fn measure_editor(app: &mut App, ui: &mut egui::Ui, id: &str, song: &Song, parsed: &ParsedSong) {
    let Some(mut draft) = app.song_ui[id].edit.clone() else { return };
    if draft.index >= song.measures.len() {
        app.song_ui.get_mut(id).unwrap().edit = None;
        return;
    }
    let number = parsed.measures.get(draft.index).map(|m| m.number).unwrap_or(0);
    let mut action: Option<&str> = None;
    ui.add_space(6.0);
    egui::Frame::group(ui.style()).inner_margin(8.0).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            ui.label(RichText::new(format!("Measure {number}  (position {})", draft.index)).strong());
            if ui.small_button("◀").clicked() {
                action = Some("prev");
            }
            if ui.small_button("▶").clicked() {
                action = Some("next");
            }
            if ui.small_button("✕").on_hover_text("Close editor").clicked() {
                action = Some("close");
            }
        });
        for (vid, (notes, lyrics)) in draft.voices.iter_mut() {
            let name = song.voice(vid).map(|v| v.name.clone()).unwrap_or_else(|| vid.clone());
            ui.horizontal(|ui| {
                ui.label(RichText::new(format!("{name:>8}")).monospace());
                ui.add(egui::TextEdit::singleline(notes).code_editor().desired_width(ui.available_width()));
            });
            if !lyrics.is_empty() || vid == song.voices.first().map(|v| v.id.as_str()).unwrap_or("") {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("  lyrics").monospace().weak());
                    ui.add(
                        egui::TextEdit::multiline(lyrics)
                            .hint_text("1: A- maz- ing\n2: 'Twas grace that")
                            .desired_rows(1)
                            .desired_width(ui.available_width()),
                    );
                });
            }
        }
        ui.horizontal_wrapped(|ui| {
            ui.checkbox(&mut draft.measure.pickup, "Pickup");
            ui.checkbox(&mut draft.measure.partial, "Partial");
            ui.checkbox(&mut draft.measure.repeat_start, "Repeat start");
            ui.label("Barline");
            egui::ComboBox::from_id_salt("barline")
                .selected_text(if draft.measure.barline.is_empty() { "normal" } else { &draft.measure.barline })
                .show_ui(ui, |ui| {
                    for b in ["", "double", "final", "repeat-end", "dashed"] {
                        ui.selectable_value(&mut draft.measure.barline, b.to_string(), if b.is_empty() { "normal" } else { b });
                    }
                });
            ui.label("Ending");
            ui.add(egui::TextEdit::singleline(&mut draft.ending).desired_width(40.0));
            ui.checkbox(&mut draft.measure.line_break, "Line break after");
        });
        ui.horizontal(|ui| {
            ui.label("Section");
            ui.add(egui::TextEdit::singleline(&mut draft.measure.section).desired_width(100.0));
            ui.label("Text");
            ui.add(egui::TextEdit::singleline(&mut draft.measure.text).desired_width(160.0));
        });
        // Live check of the draft.
        let mut test = song.clone();
        test.measures[draft.index] = draft.build();
        let (_, issues) = validate(&test);
        let here: Vec<_> = issues.iter().filter(|i| i.measure == Some(draft.index)).collect();
        for i in here.iter().take(6) {
            let color = if i.severity == Severity::Error { Color32::from_rgb(230, 90, 90) } else { Color32::from_rgb(220, 170, 60) };
            ui.label(RichText::new(format!("{} {}", i.voice.clone().unwrap_or_default(), i.message)).color(color).small());
        }
        ui.horizontal(|ui| {
            let changed = test.measures[draft.index] != song.measures[draft.index];
            if ui.add_enabled(changed, egui::Button::new(RichText::new("Apply").strong())).clicked() {
                action = Some("apply");
            }
            if ui.add_enabled(changed, egui::Button::new("Revert")).clicked() {
                action = Some("revert");
            }
            ui.separator();
            if ui.button("Insert measure after").clicked() {
                action = Some("insert");
            }
            if ui.button("Duplicate").clicked() {
                action = Some("duplicate");
            }
            if ui.button("Delete measure").clicked() {
                action = Some("delete");
            }
        });
        ui.label(
            RichText::new("Tokens: G4:q  F#4:e.  C4+E4:h  r:q  R   modifiers ~ tie  ( ) slur  ^ fermata   lyrics: \"glo- ry _ *\"")
                .small()
                .weak(),
        );
    });
    let idx = draft.index;
    app.song_ui.get_mut(id).unwrap().edit = Some(draft.clone());
    match action {
        Some("prev") if idx > 0 => select_measure(app, id, song, idx - 1),
        Some("next") if idx + 1 < song.measures.len() => select_measure(app, id, song, idx + 1),
        Some("close") => {
            let st = app.song_ui.get_mut(id).unwrap();
            st.edit = None;
            st.selected = None;
        }
        Some("revert") => select_measure(app, id, song, idx),
        Some("apply") => {
            let mut s = song.clone();
            s.measures[idx] = draft.build();
            commit_song(app, id, &format!("Edited measure {number}"), s.clone());
            select_measure(app, id, &s, idx);
        }
        Some("insert") | Some("duplicate") => {
            let mut s = song.clone();
            let new = if action == Some("duplicate") {
                s.measures[idx].clone()
            } else {
                let mut m = Measure::default();
                for v in &s.voices {
                    m.voices.insert(v.id.clone(), VoiceMeasure { notes: "R".into(), ..Default::default() });
                }
                m
            };
            s.measures.insert(idx + 1, new);
            commit_song(app, id, &format!("Inserted measure after {number}"), s.clone());
            select_measure(app, id, &s, idx + 1);
        }
        Some("delete") => {
            let mut s = song.clone();
            s.measures.remove(idx);
            commit_song(app, id, &format!("Deleted measure {number}"), s.clone());
            let st = app.song_ui.get_mut(id).unwrap();
            st.edit = None;
            st.selected = None;
        }
        _ => {}
    }
}

// ------------------------------------------------------------------ details

fn details_tab(app: &mut App, ui: &mut egui::Ui, id: &str) {
    let Some(song) = app.entry(id).and_then(|e| e.song.clone()) else {
        ui.label("Song details appear here after transcription. Until then, set the title and number on the Pages tab.");
        return;
    };
    let state = app.song_ui.get_mut(id).unwrap();
    if state.details.is_none() {
        state.details = Some(song.clone());
    }
    let mut save = false;
    let mut discard = false;
    egui::ScrollArea::vertical().id_salt("details").show(ui, |ui| {
        let d = state.details.as_mut().unwrap();
        let m = &mut d.meta;
        egui::Grid::new("details_grid").num_columns(2).spacing([10.0, 4.0]).show(ui, |ui| {
            let field = |ui: &mut egui::Ui, label: &str, v: &mut String| {
                ui.label(label);
                ui.add(egui::TextEdit::singleline(v).desired_width(420.0));
                ui.end_row();
            };
            field(ui, "Title", &mut m.title);
            field(ui, "Subtitle", &mut m.subtitle);
            field(ui, "Number", &mut m.number);
            field(ui, "First line", &mut m.first_line);
            field(ui, "Refrain first line", &mut m.refrain_first_line);
            field(ui, "Tune name", &mut m.tune_name);
            field(ui, "Meter", &mut m.meter);
            field(ui, "Lyricist", &mut m.lyricist);
            field(ui, "Composer", &mut m.composer);
            field(ui, "Arranger", &mut m.arranger);
            field(ui, "Translator", &mut m.translator);
            field(ui, "Words credit (as printed)", &mut m.words_credit);
            field(ui, "Music credit (as printed)", &mut m.music_credit);
            field(ui, "Copyright", &mut m.copyright);
            field(ui, "Scripture", &mut m.scripture);
            field(ui, "Language", &mut m.language);
            ui.label("Topics");
            let mut topics = m.topics.join(", ");
            if ui.add(egui::TextEdit::singleline(&mut topics).hint_text("comma separated").desired_width(420.0)).changed() {
                m.topics = topics.split(',').map(|t| t.trim().to_string()).filter(|t| !t.is_empty()).collect();
            }
            ui.end_row();
            ui.label("Other printed text");
            ui.add(egui::TextEdit::multiline(&mut m.other_text).desired_rows(2).desired_width(420.0));
            ui.end_row();
            ui.label("Key");
            ui.horizontal(|ui| {
                key_picker(ui, &mut d.key);
            });
            ui.end_row();
            ui.label("Time signature");
            ui.add(egui::TextEdit::singleline(&mut d.time).desired_width(60.0));
            ui.end_row();
            ui.label("Tempo");
            ui.horizontal(|ui| {
                let t = d.tempo.get_or_insert_with(Tempo::default);
                ui.add(egui::TextEdit::singleline(&mut t.text).hint_text("marking").desired_width(140.0));
                let mut bpm = t.bpm.unwrap_or(0.0);
                if ui.add(egui::DragValue::new(&mut bpm).range(0.0..=240.0).prefix("♩ = ")).changed() {
                    t.bpm = if bpm > 0.0 { Some(bpm) } else { None };
                    t.suggested = false;
                    if t.beat.is_empty() {
                        t.beat = "q".into();
                    }
                }
                if t.suggested {
                    ui.label(RichText::new("(suggested)").weak());
                }
            });
            ui.end_row();
            ui.label("Verses under the music");
            ui.add(egui::DragValue::new(&mut d.verse_count).range(1..=12));
            ui.end_row();
        });
        ui.add_space(6.0);
        ui.label(RichText::new("Stanzas printed below the music").strong());
        let mut remove = None;
        for (i, st) in d.extra_stanzas.iter_mut().enumerate() {
            ui.horizontal(|ui| {
                ui.add(egui::DragValue::new(&mut st.number).range(1..=30));
                ui.add(egui::TextEdit::multiline(&mut st.text).desired_rows(3).desired_width(420.0));
                if ui.small_button("🗑").clicked() {
                    remove = Some(i);
                }
            });
        }
        if let Some(i) = remove {
            d.extra_stanzas.remove(i);
        }
        if ui.button("➕ Add stanza").clicked() {
            let n = d.verse_count + d.extra_stanzas.len() as u32 + 1;
            d.extra_stanzas.push(Stanza { number: n, text: String::new() });
        }
        ui.add_space(8.0);
        let changed = state.details.as_ref() != Some(&song);
        ui.horizontal(|ui| {
            if ui.add_enabled(changed, egui::Button::new(RichText::new("Save details").strong())).clicked() {
                save = true;
            }
            if ui.add_enabled(changed, egui::Button::new("Discard changes")).clicked() {
                discard = true;
            }
        });
    });
    if save {
        let mut s = app.song_ui.get_mut(id).unwrap().details.take().unwrap();
        // Keep measures from the current song in case it changed meanwhile.
        if let Some(cur) = app.entry(id).and_then(|e| e.song.clone()) {
            s.measures = cur.measures;
        }
        commit_song(app, id, "Edited details", s);
    }
    if discard {
        app.song_ui.get_mut(id).unwrap().details = None;
    }
    // Refresh the draft when the song changed elsewhere.
    if let Some(d) = &app.song_ui[id].details {
        if d.measures != song.measures {
            app.song_ui.get_mut(id).unwrap().details = None;
        }
    }
}

fn key_picker(ui: &mut egui::Ui, key: &mut KeySig) {
    egui::ComboBox::from_id_salt("key_fifths").selected_text(key.name()).show_ui(ui, |ui| {
        for f in -7..=7 {
            let k = KeySig { fifths: f, mode: key.mode.clone() };
            let label = format!(
                "{} ({})",
                k.name(),
                if f == 0 {
                    "no sharps or flats".to_string()
                } else if f > 0 {
                    format!("{f}♯")
                } else {
                    format!("{}♭", -f)
                }
            );
            ui.selectable_value(&mut key.fifths, f, label);
        }
    });
    egui::ComboBox::from_id_salt("key_mode").selected_text(key.mode.clone()).show_ui(ui, |ui| {
        ui.selectable_value(&mut key.mode, "major".to_string(), "major");
        ui.selectable_value(&mut key.mode, "minor".to_string(), "minor");
    });
}

// ------------------------------------------------------------------ export

fn export_tab(app: &mut App, ui: &mut egui::Ui, id: &str) {
    let Some(song) = app.entry(id).and_then(|e| e.song.clone()) else {
        ui.label("Transcribe the song first.");
        return;
    };
    let default = app.cfg.export.clone();
    let state = app.song_ui.get_mut(id).unwrap();
    let settings = state.export.get_or_insert(default);
    let mut save_default = false;
    egui::ScrollArea::vertical().id_salt("export_tab").show(ui, |ui| {
        egui::Frame::group(ui.style()).inner_margin(10.0).show(ui, |ui| {
            ui.set_width(ui.available_width());
            super::settings::export_settings_ui(ui, settings, Some(&song.voices), Some(&song.key), "song");
            if ui.small_button("Use these options as the default").clicked() {
                save_default = true;
            }
        });
    });
    let settings = state.export.clone().unwrap();
    if save_default {
        app.cfg.export = settings.clone();
        app.save_config();
        app.toast(ToastKind::Success, "Saved as default export options");
    }
    ui.add_space(8.0);
    let errors = app.entry(id).map(|e| e.issues.iter().filter(|i| i.severity == Severity::Error).count()).unwrap_or(0);
    if errors > 0 {
        ui.label(
            RichText::new(format!("⚠ The transcription still has {errors} error(s). Exported files may have wrong measure lengths."))
                .color(Color32::from_rgb(220, 170, 60)),
        );
    }
    if (settings.mscz || settings.pdf) && app.musescore.is_none() {
        ui.label(
            RichText::new("MuseScore was not found: .mscz and PDF need MuseScore Studio installed. MusicXML works without it.").weak(),
        );
    }
    ui.horizontal(|ui| {
        if ui.button(RichText::new("💾 Export").strong().size(16.0)).clicked() {
            let p = app.project.as_ref().unwrap();
            let kind = JobKind::Export {
                project: Box::new(p.clone()),
                song_ids: vec![id.to_string()],
                settings: settings.clone(),
                musescore: app.musescore.clone(),
            };
            app.submit(JobTag::Export, format!("Export \"{}\"", song.display_title()), kind);
        }
        if ui.button("📂 Open exports folder").clicked() {
            let dir = crate::export::output_dir(app.project.as_ref().unwrap(), &settings);
            let _ = std::fs::create_dir_all(&dir);
            let _ = crate::export::musescore::open_with_default_app(&dir);
        }
        if ui.button("🎹 Play MIDI").clicked() {
            play_midi(app, id);
        }
        if ui.button("🎼 Open in MuseScore").clicked() {
            open_in_musescore(app, id);
        }
    });
    let log: Vec<String> = app.book_ui.export_log.iter().rev().take(12).cloned().collect();
    if !log.is_empty() {
        ui.add_space(6.0);
        for l in log {
            ui.label(RichText::new(l).small().monospace());
        }
    }
}
