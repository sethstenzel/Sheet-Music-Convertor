//! Settings window and the shared export-options editor.

use super::{App, View};
use crate::config::{Accuracy, VoiceLayout};
use crate::export::{ExportSettings, ScoreLayout, VerseMode, GM_INSTRUMENTS};
use crate::model::score::{KeySig, VoiceDef};
use eframe::egui::{self, RichText};

pub fn settings_window(app: &mut App, ctx: &egui::Context) {
    if !app.show_settings {
        return;
    }
    let mut open = true;
    let mut changed = false;
    egui::Window::new("Settings").open(&mut open).default_width(560.0).show(ctx, |ui| {
        egui::ScrollArea::vertical().max_height(620.0).show(ui, |ui| {
            ui.label(RichText::new("Claude").strong());
            ui.horizontal(|ui| {
                ui.label(format!("Connection: {}", app.cfg.backend.label()));
                if ui.button("Account…").clicked() {
                    app.view = View::Account;
                }
            });
            egui::Grid::new("settings_claude").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
                ui.label("Model");
                egui::ComboBox::from_id_salt("set_model")
                    .selected_text(
                        crate::claude::MODEL_CHOICES.iter().find(|m| m.0 == app.cfg.model).map(|m| m.1).unwrap_or(&app.cfg.model),
                    )
                    .show_ui(ui, |ui| {
                        for (v, l) in crate::claude::MODEL_CHOICES {
                            changed |= ui.selectable_value(&mut app.cfg.model, v.to_string(), *l).changed();
                        }
                    });
                ui.end_row();
                ui.label("Effort");
                egui::ComboBox::from_id_salt("set_effort").selected_text(app.cfg.effort.clone()).show_ui(ui, |ui| {
                    for e in crate::claude::EFFORT_CHOICES {
                        changed |= ui.selectable_value(&mut app.cfg.effort, e.to_string(), *e).changed();
                    }
                });
                ui.end_row();
                ui.label("Accuracy");
                egui::ComboBox::from_id_salt("set_acc").selected_text(app.cfg.accuracy.label()).show_ui(ui, |ui| {
                    for a in Accuracy::ALL {
                        changed |= ui.selectable_value(&mut app.cfg.accuracy, a, a.label()).changed();
                    }
                });
                ui.end_row();
                ui.label("Voice layout");
                egui::ComboBox::from_id_salt("set_layout").selected_text(app.cfg.voice_layout.label()).show_ui(ui, |ui| {
                    for l in VoiceLayout::ALL {
                        changed |= ui.selectable_value(&mut app.cfg.voice_layout, l, l.label()).changed();
                    }
                });
                ui.end_row();
                ui.label("Auto-fix rounds");
                changed |= ui.add(egui::Slider::new(&mut app.cfg.max_fix_rounds, 0..=5)).changed();
                ui.end_row();
                ui.label("Image size sent (px)");
                changed |= ui.add(egui::Slider::new(&mut app.cfg.max_image_dim, 1000..=3000).step_by(100.0)).changed();
                ui.end_row();
                ui.label("Songs at the same time");
                changed |= ui
                    .add(egui::Slider::new(&mut app.cfg.parallel_jobs, 1..=4))
                    .on_hover_text("Higher values finish batches sooner but use your plan's limits faster.")
                    .changed();
                ui.end_row();
                ui.label("Timeout per request (s)");
                changed |= ui.add(egui::Slider::new(&mut app.cfg.timeout_secs, 120..=3600).step_by(60.0)).changed();
                ui.end_row();
            });
            ui.separator();
            ui.label(RichText::new("MuseScore").strong());
            match &app.musescore {
                Some(p) => {
                    ui.label(format!("✔ {}", p.display()));
                }
                None => {
                    ui.label("MuseScore Studio was not found.");
                    ui.label(RichText::new(crate::export::musescore::INSTALL_HELP).weak());
                    ui.hyperlink_to("Download MuseScore Studio", "https://musescore.org/download");
                }
            }
            ui.horizontal(|ui| {
                if ui.button("Detect again").clicked() {
                    app.musescore = crate::export::musescore::find(&app.cfg.musescore_path);
                }
                if ui.button("Choose MuseScore…").clicked() {
                    if let Some(p) = rfd::FileDialog::new().set_title("Locate MuseScore").pick_file() {
                        app.cfg.musescore_path = p.display().to_string();
                        app.musescore = crate::export::musescore::find(&app.cfg.musescore_path);
                        changed = true;
                    }
                }
                if !app.cfg.musescore_path.is_empty() && ui.button("Use auto-detect").clicked() {
                    app.cfg.musescore_path.clear();
                    app.musescore = crate::export::musescore::find("");
                    changed = true;
                }
            });
            ui.separator();
            ui.label(RichText::new("Default export options").strong());
            changed |= export_settings_ui(ui, &mut app.cfg.export, None, None, "defaults");
        });
    });
    if changed {
        app.save_config();
    }
    if !open {
        app.show_settings = false;
        app.save_config();
    }
}

/// Edit export settings. Returns true when anything changed.
pub fn export_settings_ui(ui: &mut egui::Ui, s: &mut ExportSettings, voices: Option<&[VoiceDef]>, key: Option<&KeySig>, id: &str) -> bool {
    let mut changed = false;
    ui.horizontal_wrapped(|ui| {
        changed |= ui.checkbox(&mut s.mscz, "MuseScore (.mscz)").changed();
        changed |= ui.checkbox(&mut s.musicxml, "MusicXML").changed();
        changed |= ui.checkbox(&mut s.pdf, "PDF").changed();
        changed |= ui.checkbox(&mut s.midi, "Piano MIDI").changed();
        changed |= ui
            .checkbox(&mut s.midi_parts, "Part-practice MIDI")
            .on_hover_text("One MIDI file per voice with that voice brought out — for choir rehearsal.")
            .changed();
        changed |= ui.checkbox(&mut s.lyrics_txt, "Lyrics (.txt)").changed();
    });
    egui::Grid::new(format!("export_grid_{id}")).num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
        ui.label("Score layout");
        egui::ComboBox::from_id_salt(format!("layout_{id}")).selected_text(s.xml.layout.label()).show_ui(ui, |ui| {
            for l in [ScoreLayout::Hymn, ScoreLayout::OpenScore] {
                changed |= ui.selectable_value(&mut s.xml.layout, l, l.label()).changed();
            }
        });
        ui.end_row();
        ui.label("Transpose");
        ui.horizontal(|ui| {
            changed |= ui.add(egui::DragValue::new(&mut s.transpose_semitones).range(-11..=11).suffix(" semitones")).changed();
            if let Some(k) = key {
                if s.transpose_semitones != 0 {
                    let nk = crate::model::transpose::transpose_key_name(k, s.transpose_semitones);
                    ui.label(format!("{} → {}", k.name(), nk.name()));
                } else {
                    ui.label(RichText::new(format!("original key: {}", k.name())).weak());
                }
            }
        });
        ui.end_row();
        ui.label("Score text");
        ui.horizontal_wrapped(|ui| {
            changed |= ui.checkbox(&mut s.xml.verse_numbers, "Verse numbers").changed();
            changed |= ui.checkbox(&mut s.xml.keep_line_breaks, "Keep printed line breaks").changed();
            changed |= ui.checkbox(&mut s.xml.credits, "Title & credits").changed();
            changed |= ui.checkbox(&mut s.xml.extra_stanzas, "Extra stanzas").changed();
        });
        ui.end_row();
        ui.label("MIDI instrument");
        let current = GM_INSTRUMENTS
            .iter()
            .find(|(p, _)| *p == s.midi_opts.program)
            .map(|(_, n)| n.to_string())
            .unwrap_or_else(|| format!("Program {}", s.midi_opts.program));
        egui::ComboBox::from_id_salt(format!("instr_{id}")).selected_text(current).show_ui(ui, |ui| {
            for (p, n) in GM_INSTRUMENTS {
                changed |= ui.selectable_value(&mut s.midi_opts.program, *p, *n).changed();
            }
        });
        ui.end_row();
        ui.label("MIDI verses");
        ui.horizontal(|ui| {
            let mut mode = match s.midi_opts.verses {
                VerseMode::Once => 0,
                VerseMode::AllVerses => 1,
                VerseMode::Times(_) => 2,
            };
            let before = mode;
            ui.radio_value(&mut mode, 0, "Once");
            ui.radio_value(&mut mode, 1, "Every verse");
            ui.radio_value(&mut mode, 2, "Times:");
            let mut n = if let VerseMode::Times(n) = s.midi_opts.verses { n } else { 2 };
            let n_changed = ui.add_enabled(mode == 2, egui::DragValue::new(&mut n).range(1..=12)).changed();
            if before != mode || n_changed {
                s.midi_opts.verses = match mode {
                    0 => VerseMode::Once,
                    1 => VerseMode::AllVerses,
                    _ => VerseMode::Times(n),
                };
                changed = true;
            }
        });
        ui.end_row();
        ui.label("Introduction");
        changed |= ui.add(egui::DragValue::new(&mut s.midi_opts.intro_measures).range(0..=16).suffix(" measures (from the end)")).changed();
        ui.end_row();
        ui.label("Tempo");
        ui.horizontal(|ui| {
            let mut custom = s.midi_opts.tempo_bpm.is_some();
            if ui.checkbox(&mut custom, "Custom").changed() {
                s.midi_opts.tempo_bpm = if custom { Some(96.0) } else { None };
                changed = true;
            }
            if let Some(bpm) = s.midi_opts.tempo_bpm.as_mut() {
                changed |= ui.add(egui::DragValue::new(bpm).range(30.0..=240.0).suffix(" ♩/min")).changed();
            } else {
                ui.label(RichText::new("from the score").weak());
            }
        });
        ui.end_row();
        ui.label("Expression");
        ui.horizontal_wrapped(|ui| {
            ui.label("Fermata ×");
            changed |= ui.add(egui::DragValue::new(&mut s.midi_opts.fermata_factor).range(1.0..=4.0).speed(0.05)).changed();
            changed |= ui.checkbox(&mut s.midi_opts.final_ritard, "Slow down at the end").changed();
            changed |= ui.checkbox(&mut s.midi_opts.count_in, "Count-in").changed();
            changed |= ui.checkbox(&mut s.midi_opts.expand_repeats, "Play repeats").changed();
        });
        ui.end_row();
        if let Some(voices) = voices {
            ui.label("Bring out voice");
            let label = s.midi_opts.emphasize_voice.clone().unwrap_or_else(|| "None".into());
            egui::ComboBox::from_id_salt(format!("emph_{id}")).selected_text(label).show_ui(ui, |ui| {
                changed |= ui.selectable_value(&mut s.midi_opts.emphasize_voice, None, "None").changed();
                for v in voices {
                    changed |= ui
                        .selectable_value(&mut s.midi_opts.emphasize_voice, Some(v.id.clone()), format!("{} ({})", v.name, v.id))
                        .changed();
                }
            });
            ui.end_row();
            ui.label("Leave out voices");
            ui.horizontal_wrapped(|ui| {
                for v in voices {
                    let mut muted = s.midi_opts.mute_voices.contains(&v.id);
                    if ui.checkbox(&mut muted, &v.name).changed() {
                        if muted {
                            s.midi_opts.mute_voices.push(v.id.clone());
                        } else {
                            s.midi_opts.mute_voices.retain(|x| x != &v.id);
                        }
                        changed = true;
                    }
                }
            });
            ui.end_row();
        }
        ui.label("File names");
        changed |= ui
            .add(egui::TextEdit::singleline(&mut s.filename_pattern).hint_text("{number} {title}"))
            .on_hover_text("Placeholders: {number}, {title}, {book}")
            .changed();
        ui.end_row();
        ui.label("Export folder");
        ui.horizontal(|ui| {
            let shown = if s.output_dir.is_empty() { "project's exports folder".to_string() } else { s.output_dir.clone() };
            ui.label(RichText::new(shown).small());
            if ui.button("Change…").clicked() {
                if let Some(d) = rfd::FileDialog::new().pick_folder() {
                    s.output_dir = d.display().to_string();
                    changed = true;
                }
            }
            if !s.output_dir.is_empty() && ui.button("Reset").clicked() {
                s.output_dir.clear();
                changed = true;
            }
        });
        ui.end_row();
    });
    changed
}
