//! Home screen, new-project wizard and help.

use super::{App, SongTab, ToastKind, View};
use crate::config::default_projects_dir;
use crate::export::sanitize_filename;
use crate::model::project::{BookInfo, CollectionKind, Project, SongEntry};
use eframe::egui::{self, RichText};

#[derive(Default)]
pub struct Wizard {
    pub open: bool,
    step: u8,
    kind: CollectionKind,
    name: String,
    location: String,
    book: BookInfo,
    song_title: String,
    error: Option<String>,
}

pub fn home(app: &mut App, ui: &mut egui::Ui) {
    egui::ScrollArea::vertical().id_salt("home").show(ui, |ui| {
        ui.add_space(24.0);
        ui.vertical_centered(|ui| {
            ui.label(RichText::new("🎼 Sheet Music Convertor").size(30.0).strong());
            ui.label(RichText::new("Turn photos of hymnals and sheet music into MuseScore scores, MIDI and lyrics — powered by Claude.").size(15.0));
        });
        ui.add_space(20.0);
        ui.vertical_centered(|ui| {
            ui.set_max_width(640.0);
            if !app.backend_ready() {
                egui::Frame::group(ui.style()).inner_margin(12.0).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.label(RichText::new("First, connect your Claude account").strong());
                    ui.label("Sign in with your Claude subscription (through Claude Code) or add an API key.");
                    if ui.button("Open account settings").clicked() {
                        app.view = View::Account;
                    }
                });
                ui.add_space(12.0);
            }
            ui.horizontal(|ui| {
                if ui.add(egui::Button::new(RichText::new("➕  New project").size(18.0)).min_size(egui::vec2(200.0, 44.0))).clicked() {
                    app.wizard = Wizard { open: true, ..Default::default() };
                }
                if ui.add(egui::Button::new(RichText::new("📂  Open project…").size(18.0)).min_size(egui::vec2(200.0, 44.0))).clicked() {
                    if let Some(dir) = rfd::FileDialog::new().set_title("Open project folder").pick_folder() {
                        app.open_project(&dir);
                    }
                }
            });
            ui.add_space(16.0);
            let recents = app.cfg.recent_projects.clone();
            if !recents.is_empty() {
                ui.label(RichText::new("Recent projects").strong());
                for r in recents {
                    let label = r.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| r.display().to_string());
                    if ui.link(label).on_hover_text(r.display().to_string()).clicked() {
                        app.open_project(&r);
                    }
                }
                ui.add_space(16.0);
            }
            egui::Frame::group(ui.style()).inner_margin(12.0).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(RichText::new("How it works").strong());
                for line in [
                    "1. Create a project for a hymnal, song book, or a single song sheet.",
                    "2. Add a song and its page photos one page at a time (or drop many pages into the inbox and let Claude find the songs).",
                    "3. Mark parts of other songs at the top or bottom of a page to be ignored.",
                    "4. Transcribe: Claude reads every note, verse and credit, then the app checks the result and has Claude fix and proofread it.",
                    "5. Review side by side with the photo, ask for corrections in plain words, and export MuseScore, MusicXML, PDF, MIDI and lyrics.",
                ] {
                    ui.label(line);
                }
            });
        });
    });
}

pub fn wizard_window(app: &mut App, ctx: &egui::Context) {
    if !app.wizard.open {
        return;
    }
    let mut open = true;
    let mut create = false;
    egui::Window::new("New project")
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .default_width(560.0)
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .show(ctx, |ui| {
            let w = &mut app.wizard;
            match w.step {
                0 => {
                    ui.label(RichText::new("What are you converting?").size(18.0).strong());
                    ui.add_space(6.0);
                    for kind in CollectionKind::ALL {
                        let desc = match kind {
                            CollectionKind::SoloSheet => {
                                "One song on one or more loose pages (sheet music, a bulletin insert, a choir part)."
                            }
                            CollectionKind::Hymnal => "Songs taken from a hymnal. The book's details are saved with every song.",
                            CollectionKind::SongBook => "A song book, chorus book or choir collection.",
                            CollectionKind::Other => "Anything else — a manuscript, a worship folder, a method book.",
                        };
                        let resp =
                            ui.add(egui::Button::new(RichText::new(kind.label()).size(16.0).strong()).min_size(egui::vec2(520.0, 34.0)));
                        ui.label(RichText::new(desc).weak());
                        ui.add_space(4.0);
                        if resp.clicked() {
                            w.kind = kind;
                            w.step = 1;
                        }
                    }
                }
                _ => {
                    ui.label(RichText::new(w.kind.label()).size(16.0).strong());
                    ui.add_space(4.0);
                    egui::Grid::new("wiz_grid").num_columns(2).spacing([10.0, 6.0]).show(ui, |ui| {
                        if w.kind.is_book() {
                            ui.label("Book title");
                            ui.add(egui::TextEdit::singleline(&mut w.book.title).hint_text("e.g. The Hymnal 1982").desired_width(340.0));
                            ui.end_row();
                            ui.label("Publisher");
                            ui.text_edit_singleline(&mut w.book.publisher);
                            ui.end_row();
                            ui.label("Year");
                            ui.text_edit_singleline(&mut w.book.year);
                            ui.end_row();
                            ui.label("Edition");
                            ui.text_edit_singleline(&mut w.book.edition);
                            ui.end_row();
                            ui.label("Denomination / tradition");
                            ui.text_edit_singleline(&mut w.book.denomination);
                            ui.end_row();
                            ui.label("Language");
                            ui.add(egui::TextEdit::singleline(&mut w.book.language).hint_text("English"));
                            ui.end_row();
                        } else {
                            ui.label("Song title (optional)");
                            ui.add(egui::TextEdit::singleline(&mut w.song_title).desired_width(340.0));
                            ui.end_row();
                        }
                        ui.label("Project name");
                        let hint = suggested_name(w);
                        ui.add(egui::TextEdit::singleline(&mut w.name).hint_text(hint).desired_width(340.0));
                        ui.end_row();
                        ui.label("Save in");
                        ui.horizontal(|ui| {
                            let shown =
                                if w.location.is_empty() { default_projects_dir().display().to_string() } else { w.location.clone() };
                            ui.label(RichText::new(shown).small());
                            if ui.button("Change…").clicked() {
                                if let Some(d) = rfd::FileDialog::new().set_title("Folder for the project").pick_folder() {
                                    w.location = d.display().to_string();
                                }
                            }
                        });
                        ui.end_row();
                    });
                    if w.kind.is_book() {
                        ui.label(
                            RichText::new("You can fill in the rest later — or let Claude read it from a photo of the title page.").weak(),
                        );
                    }
                    if let Some(e) = &w.error {
                        ui.colored_label(egui::Color32::from_rgb(220, 80, 80), e);
                    }
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button("← Back").clicked() {
                            w.step = 0;
                        }
                        if ui.button(RichText::new("Create project").strong()).clicked() {
                            create = true;
                        }
                    });
                }
            }
        });
    if !open {
        app.wizard.open = false;
    }
    if create {
        create_project(app);
    }
}

fn suggested_name(w: &Wizard) -> String {
    if w.kind.is_book() && !w.book.title.trim().is_empty() {
        w.book.title.trim().to_string()
    } else if !w.song_title.trim().is_empty() {
        w.song_title.trim().to_string()
    } else {
        match w.kind {
            CollectionKind::SoloSheet => "My song".into(),
            CollectionKind::Hymnal => "My hymnal".into(),
            CollectionKind::SongBook => "My song book".into(),
            CollectionKind::Other => "My project".into(),
        }
    }
}

fn create_project(app: &mut App) {
    let w = &mut app.wizard;
    let name = if w.name.trim().is_empty() { suggested_name(w) } else { w.name.trim().to_string() };
    let base = if w.location.is_empty() { default_projects_dir() } else { std::path::PathBuf::from(&w.location) };
    let mut dir = base.join(sanitize_filename(&name));
    let mut n = 2;
    while dir.join(crate::model::project::PROJECT_FILE).exists() {
        dir = base.join(format!("{} ({n})", sanitize_filename(&name)));
        n += 1;
    }
    match Project::create(&dir, &name, w.kind) {
        Ok(mut p) => {
            p.book = w.book.clone();
            if !w.kind.is_book() {
                let e = SongEntry { title_hint: w.song_title.trim().to_string(), ..Default::default() };
                p.songs.push(e);
            }
            if let Err(e) = p.save() {
                w.error = Some(format!("{e:#}"));
                return;
            }
            let solo = !p.kind.is_book();
            let first = p.songs.first().map(|s| s.id.clone());
            app.wizard.open = false;
            app.open_project(&dir);
            if let (true, Some(id)) = (solo, first) {
                app.view = View::Song(id);
                app.song_tab = SongTab::Pages;
            }
            app.toast(ToastKind::Success, format!("Created project in {}", dir.display()));
        }
        Err(e) => w.error = Some(format!("{e:#}")),
    }
}

pub fn help_window(app: &mut App, ctx: &egui::Context) {
    if !app.show_help {
        return;
    }
    egui::Window::new("Getting started").open(&mut app.show_help).default_width(620.0).show(ctx, |ui| {
        egui::ScrollArea::vertical().max_height(520.0).show(ui, |ui| {
            ui.label(RichText::new("Taking good photos").strong());
            for l in [
                "• Lay the book flat in even light; avoid shadows from your phone and the page curve near the spine.",
                "• Fill the frame with one page and hold the phone parallel to the page. Use the highest resolution.",
                "• Make sure every staff, the title, and the credit lines at the top and bottom are in the picture.",
                "• Use the crop tool to remove the facing page or the table, and 'Improve contrast' for yellowed paper.",
            ] {
                ui.label(l);
            }
            ui.add_space(6.0);
            ui.label(RichText::new("Songs over several pages").strong());
            ui.label(
                "Add each page to the song one at a time in order. If a page also shows the end of the previous song at the \
                 top, tick 'Ignore song at top'. If the next song starts at the bottom, tick 'Ignore song at bottom'. For a \
                 page with three or more songs, add a page note such as 'only the second song, #245'.",
            );
            ui.add_space(6.0);
            ui.label(RichText::new("Accuracy").strong());
            ui.label(
                "Every transcription is checked automatically: measure lengths, lyric alignment, voice ranges and ties. \
                 Errors are sent back to Claude to fix, and in High/Maximum accuracy Claude proofreads the result against \
                 the photos again. Always review the result: the Review tab shows the photo next to the transcription; \
                 with MuseScore installed it also shows the engraved notation. Ask for corrections in plain words, e.g. \
                 'measure 6: the alto's second note is B♭, not C'.",
            );
            ui.add_space(6.0);
            ui.label(RichText::new("Exports").strong());
            ui.label(
                "MusicXML opens directly in MuseScore Studio. If MuseScore is installed the app also saves native .mscz \
                 files and PDFs. Piano MIDI can play every verse with an introduction, and 'part practice' MIDI files bring \
                 out one voice for choir rehearsal. Transpose to any key on export.",
            );
        });
    });
}
