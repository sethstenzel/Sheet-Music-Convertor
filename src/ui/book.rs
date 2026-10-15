//! Book dashboard (details, batch actions, catalog) and the page inbox.

use super::widgets::Tex;
use super::{App, JobTag, SongTab, ToastKind, View};
use crate::jobs::JobKind;
use crate::model::project::{CollectionKind, SongEntry, SongPage, SongStatus};
use crate::pipeline::DetectedSong;
use eframe::egui::{self, RichText};
use std::path::PathBuf;

#[derive(Default)]
pub struct BookUi {
    pub log: Vec<String>,
    pub export_log: Vec<String>,
    pub detected: Option<Vec<(bool, DetectedSong)>>,
    pub transcribe_after_create: bool,
    pub clear_inbox_after: bool,
}

pub fn transcribe_all_drafts(app: &mut App) {
    if !app.backend_ready() {
        app.toast(ToastKind::Error, "Sign in to Claude first (Account).");
        app.view = View::Account;
        return;
    }
    let ids: Vec<String> = app
        .project
        .as_ref()
        .map(|p| {
            p.songs
                .iter()
                .filter(|s| matches!(s.status, SongStatus::Draft | SongStatus::Failed) && !s.pages.is_empty())
                .map(|s| s.id.clone())
                .collect()
        })
        .unwrap_or_default();
    let ids: Vec<String> = ids.into_iter().filter(|id| !app.jobs.is_busy(id)).collect();
    if ids.is_empty() {
        app.toast(ToastKind::Info, "No draft songs with pages to transcribe.");
        return;
    }
    let n = ids.len();
    for id in ids {
        super::song::start_transcription(app, &id);
    }
    app.toast(ToastKind::Info, format!("Queued {n} song(s) for transcription"));
}

pub fn export_all(app: &mut App) {
    let Some(p) = &app.project else { return };
    let ids: Vec<String> = p.songs.iter().filter(|s| s.song.is_some()).map(|s| s.id.clone()).collect();
    if ids.is_empty() {
        app.toast(ToastKind::Info, "No transcribed songs to export yet.");
        return;
    }
    let n = ids.len();
    let kind =
        JobKind::Export { project: Box::new(p.clone()), song_ids: ids, settings: app.cfg.export.clone(), musescore: app.musescore_path() };
    app.submit(JobTag::Export, format!("Export {n} songs"), kind);
}

pub fn add_inbox_pages(app: &mut App, paths: &[PathBuf]) {
    let Some(p) = app.project.as_mut() else { return };
    let mut added = 0;
    let mut errors = Vec::new();
    for path in paths {
        match p.import_image(path) {
            Ok(page) => {
                p.inbox.push(page);
                added += 1;
            }
            Err(e) => errors.push(format!("{e:#}")),
        }
    }
    app.mark_dirty();
    if added > 0 {
        app.toast(ToastKind::Success, format!("Added {added} page(s) to the inbox"));
    }
    for e in errors {
        app.toast(ToastKind::Error, e);
    }
}

fn book_field(ui: &mut egui::Ui, label: &str, value: &mut String, hint: &str) -> bool {
    ui.label(label);
    let r = ui.add(egui::TextEdit::singleline(value).hint_text(hint).desired_width(320.0));
    ui.end_row();
    r.changed()
}

pub fn book_view(app: &mut App, ui: &mut egui::Ui) {
    egui::ScrollArea::vertical().id_salt("book_view").show(ui, |ui| {
        let mut changed = false;
        let (kind, counts, total_songs) = {
            let p = app.project.as_ref().unwrap();
            let mut counts = std::collections::BTreeMap::new();
            for s in &p.songs {
                *counts.entry(s.status.label()).or_insert(0) += 1;
            }
            (p.kind, counts, p.songs.len())
        };
        ui.horizontal(|ui| {
            let p = app.project.as_mut().unwrap();
            ui.heading(if p.book.title.is_empty() { p.name.clone() } else { p.book.title.clone() });
            ui.label(RichText::new(kind.label()).weak());
        });
        ui.label(RichText::new(app.project.as_ref().unwrap().dir.display().to_string()).small().weak());
        ui.add_space(6.0);

        ui.columns(2, |cols| {
            let ui = &mut cols[0];
            egui::Frame::group(ui.style()).inner_margin(10.0).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(RichText::new(if kind.is_book() { "Book details" } else { "Project" }).strong());
                let p = app.project.as_mut().unwrap();
                egui::Grid::new("book_grid").num_columns(2).spacing([10.0, 4.0]).show(ui, |ui| {
                    ui.label("Type");
                    egui::ComboBox::from_id_salt("kind").selected_text(p.kind.label()).show_ui(ui, |ui| {
                        for k in CollectionKind::ALL {
                            changed |= ui.selectable_value(&mut p.kind, k, k.label()).changed();
                        }
                    });
                    ui.end_row();
                    changed |= book_field(ui, "Project name", &mut p.name, "");
                    if p.kind.is_book() {
                        let b = &mut p.book;
                        changed |= book_field(ui, "Title", &mut b.title, "");
                        changed |= book_field(ui, "Subtitle", &mut b.subtitle, "");
                        changed |= book_field(ui, "Abbreviation", &mut b.abbreviation, "e.g. LBW, H82 (used in file names)");
                        changed |= book_field(ui, "Publisher", &mut b.publisher, "");
                        changed |= book_field(ui, "Year", &mut b.year, "");
                        changed |= book_field(ui, "Edition", &mut b.edition, "");
                        changed |= book_field(ui, "Editor / compiler", &mut b.editor, "");
                        changed |= book_field(ui, "Denomination", &mut b.denomination, "");
                        changed |= book_field(ui, "Language", &mut b.language, "English");
                        changed |= book_field(ui, "ISBN", &mut b.isbn, "");
                        changed |= book_field(ui, "Copyright", &mut b.copyright, "");
                        ui.label("Notes");
                        changed |= ui.add(egui::TextEdit::multiline(&mut b.notes).desired_rows(2).desired_width(320.0)).changed();
                        ui.end_row();
                    }
                });
                if kind.is_book() {
                    ui.add_space(4.0);
                    let busy = app.job_tags.values().any(|t| *t == JobTag::Book);
                    ui.horizontal(|ui| {
                        if ui
                            .add_enabled(!busy, egui::Button::new("📷 Fill in from title / copyright page photo…"))
                            .on_hover_text("Claude reads the book's title, publisher, year and copyright from photos.")
                            .clicked()
                        {
                            if let Some(paths) =
                                rfd::FileDialog::new().add_filter("Images", crate::imaging::SUPPORTED_EXTENSIONS).pick_files()
                            {
                                if app.backend_ready() {
                                    app.submit(JobTag::Book, "Read book details", JobKind::BookInfo { paths });
                                } else {
                                    app.toast(ToastKind::Error, "Sign in to Claude first (Account).");
                                }
                            }
                        }
                        if busy {
                            ui.spinner();
                        }
                    });
                }
            });

            let ui = &mut cols[1];
            egui::Frame::group(ui.style()).inner_margin(10.0).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(RichText::new("Songs").strong());
                ui.label(format!("{total_songs} song(s)"));
                for (label, n) in &counts {
                    ui.label(format!("  {label}: {n}"));
                }
                ui.add_space(6.0);
                if ui.button("➕ New song").clicked() {
                    app.add_song();
                }
                if kind.is_book() && ui.button("📥 Add many pages and detect songs…").clicked() {
                    app.view = View::Inbox;
                }
                if ui.button("▶ Transcribe all drafts").clicked() {
                    transcribe_all_drafts(app);
                }
                if ui.button("💾 Export all transcribed songs").clicked() {
                    export_all(app);
                }
                ui.horizontal(|ui| {
                    if ui
                        .button("Catalog (.csv)")
                        .on_hover_text("Spreadsheet of all songs with numbers, titles, tunes, meters and credits")
                        .clicked()
                    {
                        save_text_export(app, "catalog.csv", crate::export::catalog::catalog_csv);
                    }
                    if ui
                        .button("Index (.txt)")
                        .on_hover_text("Indexes of titles, first lines, tunes and meters, like the back of a hymnal")
                        .clicked()
                    {
                        save_text_export(app, "index.txt", crate::export::catalog::index_text);
                    }
                });
                if ui.button("📂 Open exports folder").clicked() {
                    let p = app.project.as_ref().unwrap();
                    let dir = crate::export::output_dir(p, &app.cfg.export);
                    let _ = std::fs::create_dir_all(&dir);
                    let _ = crate::export::musescore::open_with_default_app(&dir);
                }
                let usage = app.project.as_ref().unwrap().usage.clone();
                if usage.requests > 0 {
                    ui.add_space(6.0);
                    ui.label(
                        RichText::new(format!(
                            "Claude usage: {} requests, {}k tokens in, {}k out",
                            usage.requests,
                            usage.input_tokens / 1000,
                            usage.output_tokens / 1000
                        ))
                        .weak(),
                    );
                }
            });
        });

        ui.add_space(8.0);
        egui::CollapsingHeader::new("Export options for batch exports").id_salt("book_export_opts").show(ui, |ui| {
            if super::settings::export_settings_ui(ui, &mut app.cfg.export, None, None, "book") {
                app.save_config();
            }
        });
        if !app.book_ui.export_log.is_empty() {
            egui::CollapsingHeader::new(format!("Export log ({})", app.book_ui.export_log.len())).id_salt("export_log").show(ui, |ui| {
                for l in app.book_ui.export_log.iter().rev().take(200) {
                    ui.label(RichText::new(l).small().monospace());
                }
                if ui.button("Clear").clicked() {
                    app.book_ui.export_log.clear();
                }
            });
        }

        ui.add_space(8.0);
        song_table(app, ui);
        if changed {
            app.mark_dirty();
        }
    });
}

fn save_text_export(app: &mut App, name: &str, make: impl Fn(&crate::model::project::Project) -> String) {
    let Some(p) = &app.project else { return };
    let dir = crate::export::output_dir(p, &app.cfg.export);
    let path = dir.join(name);
    let text = make(p);
    match std::fs::create_dir_all(&dir).and_then(|_| std::fs::write(&path, text)) {
        Ok(()) => {
            app.toast(ToastKind::Success, format!("Saved {}", path.display()));
            app.book_ui.export_log.push(format!("Saved {}", path.display()));
        }
        Err(e) => app.toast(ToastKind::Error, format!("Could not save {}: {e}", path.display())),
    }
}

fn song_table(app: &mut App, ui: &mut egui::Ui) {
    let rows: Vec<(String, String, String, String, usize, usize, String, String)> = app
        .project
        .as_ref()
        .unwrap()
        .songs
        .iter()
        .map(|s| {
            (
                s.id.clone(),
                s.display_number(),
                s.display_title(),
                s.status.label().to_string(),
                s.pages.len(),
                s.issues.len(),
                s.song.as_ref().map(|x| x.meta.tune_name.clone()).unwrap_or_default(),
                s.book_pages(),
            )
        })
        .collect();
    if rows.is_empty() {
        return;
    }
    egui::Frame::group(ui.style()).inner_margin(8.0).show(ui, |ui| {
        ui.set_width(ui.available_width());
        egui::Grid::new("song_table").striped(true).num_columns(7).spacing([14.0, 4.0]).show(ui, |ui| {
            for h in ["No.", "Title", "Tune", "Pages", "Status", "Issues", ""] {
                ui.label(RichText::new(h).strong());
            }
            ui.end_row();
            for (id, num, title, status, pages, issues, tune, book_pages) in rows {
                ui.label(num);
                ui.label(title);
                ui.label(tune);
                ui.label(if book_pages.is_empty() { format!("{pages} img") } else { format!("p. {book_pages}") });
                ui.label(status);
                ui.label(if issues > 0 { issues.to_string() } else { String::new() });
                if ui.small_button("Open").clicked() {
                    let has = app.entry(&id).map(|e| e.song.is_some()).unwrap_or(false);
                    app.song_tab = if has { SongTab::Review } else { SongTab::Pages };
                    app.view = View::Song(id);
                }
                ui.end_row();
            }
        });
    });
}

pub fn inbox_view(app: &mut App, ui: &mut egui::Ui) {
    ui.heading("Page inbox");
    ui.label(
        "Add photos of many consecutive pages at once. Claude finds which songs are on which pages — including songs that \
         continue onto the next page — and creates the songs for you, with the right pages and ignore-top/bottom settings.",
    );
    ui.add_space(6.0);
    let detecting = app.job_tags.values().any(|t| *t == JobTag::Detect);
    ui.horizontal(|ui| {
        if ui.button("➕ Add pages…").clicked() {
            if let Some(paths) = rfd::FileDialog::new()
                .add_filter("Images", crate::imaging::SUPPORTED_EXTENSIONS)
                .set_title("Add pages (in book order)")
                .pick_files()
            {
                let mut paths = paths;
                paths.sort();
                add_inbox_pages(app, &paths);
            }
        }
        let n = app.project.as_ref().map(|p| p.inbox.len()).unwrap_or(0);
        if ui.add_enabled(n > 0 && !detecting, egui::Button::new(RichText::new("🔎 Detect songs").strong())).clicked() {
            if !app.backend_ready() {
                app.toast(ToastKind::Error, "Sign in to Claude first (Account).");
            } else {
                let p = app.project.as_ref().unwrap();
                let kind = JobKind::DetectSongs { kind: p.kind, book: p.book.clone(), images_dir: p.images_dir(), pages: p.inbox.clone() };
                app.submit(JobTag::Detect, format!("Detect songs on {n} pages"), kind);
            }
        }
        if detecting {
            ui.spinner();
            ui.label("Claude is looking through the pages…");
        }
        if n > 0 && ui.button("Remove all pages").clicked() {
            app.project.as_mut().unwrap().inbox.clear();
            app.mark_dirty();
        }
    });
    ui.label(RichText::new("Tip: you can also drag and drop image files here. Pages are kept in the order shown.").weak());
    ui.separator();

    if app.book_ui.detected.is_some() {
        detected_panel(app, ui);
        ui.separator();
    }

    let ctx = ui.ctx().clone();
    let images_dir = app.project.as_ref().unwrap().images_dir();
    let pages: Vec<SongPage> = app.project.as_ref().unwrap().inbox.clone();
    let mut action: Option<(usize, &str)> = None;
    let mut new_pages = pages.clone();
    egui::ScrollArea::vertical().id_salt("inbox_pages").show(ui, |ui| {
        ui.horizontal_wrapped(|ui| {
            for (i, page) in pages.iter().enumerate() {
                egui::Frame::group(ui.style()).inner_margin(6.0).show(ui, |ui| {
                    ui.set_width(190.0);
                    ui.vertical(|ui| {
                        let path = images_dir.join(&page.image);
                        match app.tex.get(&ctx, &path, page.rotation, 360) {
                            Tex::Ready(t) => {
                                super::widgets::fitted_image(ui, t, egui::vec2(180.0, 230.0), egui::Sense::hover());
                            }
                            Tex::Loading => {
                                ui.add_sized([180.0, 230.0], egui::Spinner::new());
                            }
                            Tex::Failed(e) => {
                                ui.add_sized([180.0, 60.0], egui::Label::new(RichText::new(e).small()));
                            }
                        }
                        ui.horizontal(|ui| {
                            ui.label(format!("#{}", i + 1));
                            ui.add(egui::TextEdit::singleline(&mut new_pages[i].book_page).hint_text("page no.").desired_width(60.0));
                        });
                        ui.horizontal(|ui| {
                            if ui.small_button("◀").on_hover_text("Move earlier").clicked() {
                                action = Some((i, "left"));
                            }
                            if ui.small_button("▶").on_hover_text("Move later").clicked() {
                                action = Some((i, "right"));
                            }
                            if ui.small_button("⟳").on_hover_text("Rotate").clicked() {
                                action = Some((i, "rotate"));
                            }
                            if ui.small_button("🗑").on_hover_text("Remove").clicked() {
                                action = Some((i, "remove"));
                            }
                        });
                    });
                });
            }
        });
    });
    let p = app.project.as_mut().unwrap();
    let mut changed = new_pages != pages;
    p.inbox = new_pages;
    if let Some((i, what)) = action {
        match what {
            "left" if i > 0 => p.inbox.swap(i, i - 1),
            "right" if i + 1 < p.inbox.len() => p.inbox.swap(i, i + 1),
            "rotate" => p.inbox[i].rotation = (p.inbox[i].rotation + 90) % 360,
            "remove" => {
                p.inbox.remove(i);
            }
            _ => {}
        }
        changed = true;
    }
    if changed {
        app.mark_dirty();
    }
}

fn detected_panel(app: &mut App, ui: &mut egui::Ui) {
    let mut create = false;
    let mut discard = false;
    egui::Frame::group(ui.style()).inner_margin(10.0).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.label(RichText::new("Songs found").strong());
        let detected = app.book_ui.detected.as_mut().unwrap();
        if detected.is_empty() {
            ui.label("No songs with music were found on these pages.");
        }
        egui::Grid::new("detected").striped(true).num_columns(4).spacing([12.0, 4.0]).show(ui, |ui| {
            for (keep, s) in detected.iter_mut() {
                ui.checkbox(keep, "");
                ui.label(&s.number);
                ui.label(&s.title);
                let pages: Vec<String> = s
                    .pages
                    .iter()
                    .map(|p| {
                        let mut t = if p.book_page.is_empty() { "?".to_string() } else { p.book_page.clone() };
                        if p.ignore_top {
                            t.push_str(" (lower part)");
                        } else if p.ignore_bottom {
                            t.push_str(" (upper part)");
                        }
                        t
                    })
                    .collect();
                ui.label(format!("{} {}", if pages.len() == 1 { "page" } else { "pages" }, pages.join(", ")));
                ui.end_row();
            }
        });
        ui.horizontal(|ui| {
            ui.checkbox(&mut app.book_ui.transcribe_after_create, "Transcribe them right away");
            ui.checkbox(&mut app.book_ui.clear_inbox_after, "Empty the inbox afterwards");
        });
        ui.horizontal(|ui| {
            let n = app.book_ui.detected.as_ref().unwrap().iter().filter(|d| d.0).count();
            if ui.add_enabled(n > 0, egui::Button::new(RichText::new(format!("Create {n} song(s)")).strong())).clicked() {
                create = true;
            }
            if ui.button("Discard").clicked() {
                discard = true;
            }
        });
    });
    if create {
        let detected = app.book_ui.detected.take().unwrap_or_default();
        let mut ids = Vec::new();
        if let Some(p) = app.project.as_mut() {
            for (keep, d) in detected {
                if !keep {
                    continue;
                }
                let e =
                    SongEntry { title_hint: d.title.clone(), number_hint: d.number.clone(), pages: d.pages.clone(), ..Default::default() };
                ids.push(e.id.clone());
                p.songs.push(e);
            }
            if app.book_ui.clear_inbox_after {
                p.inbox.clear();
            }
        }
        app.mark_dirty();
        let n = ids.len();
        if app.book_ui.transcribe_after_create {
            for id in &ids {
                super::song::start_transcription(app, id);
            }
            app.toast(ToastKind::Success, format!("Created and queued {n} song(s)"));
        } else {
            app.toast(ToastKind::Success, format!("Created {n} song(s)"));
        }
        app.view = View::Book;
    }
    if discard {
        app.book_ui.detected = None;
    }
}
