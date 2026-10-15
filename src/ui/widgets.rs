//! Reusable UI pieces: async image textures, page viewer with crop, piano roll.

use crate::model::notation::ParsedSong;
use eframe::egui::{self, Color32, Pos2, Rect, Sense, Stroke, StrokeKind, TextureHandle, Vec2};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};

type Key = (PathBuf, u16, u32);

/// Loads images on background threads and keeps GPU textures.
pub struct TextureCache {
    textures: HashMap<Key, TextureHandle>,
    failed: HashMap<Key, String>,
    pending: HashSet<Key>,
    tx: Sender<(Key, Result<egui::ColorImage, String>)>,
    rx: Receiver<(Key, Result<egui::ColorImage, String>)>,
}

impl Default for TextureCache {
    fn default() -> Self {
        let (tx, rx) = channel();
        Self { textures: HashMap::new(), failed: HashMap::new(), pending: HashSet::new(), tx, rx }
    }
}

pub enum Tex<'a> {
    Ready(&'a TextureHandle),
    Loading,
    Failed(String),
}

impl TextureCache {
    /// Receive finished loads. Call once per frame.
    pub fn pump(&mut self, ctx: &egui::Context) {
        while let Ok((key, res)) = self.rx.try_recv() {
            self.pending.remove(&key);
            match res {
                Ok(img) => {
                    let name = format!("{}#{}#{}", key.0.display(), key.1, key.2);
                    let tex = ctx.load_texture(name, img, egui::TextureOptions::LINEAR);
                    self.textures.insert(key, tex);
                }
                Err(e) => {
                    self.failed.insert(key, e);
                }
            }
        }
    }

    pub fn get(&mut self, ctx: &egui::Context, path: &Path, rotation: u16, max_dim: u32) -> Tex<'_> {
        let key = (path.to_path_buf(), rotation, max_dim);
        if self.textures.contains_key(&key) {
            return Tex::Ready(&self.textures[&key]);
        }
        if let Some(e) = self.failed.get(&key) {
            return Tex::Failed(e.clone());
        }
        if self.pending.insert(key.clone()) {
            let tx = self.tx.clone();
            let ctx = ctx.clone();
            std::thread::spawn(move || {
                let res = crate::imaging::thumbnail(&key.0, key.1, key.2)
                    .map(|rgba| {
                        let size = [rgba.width() as usize, rgba.height() as usize];
                        egui::ColorImage::from_rgba_unmultiplied(size, rgba.as_raw())
                    })
                    .map_err(|e| format!("{e:#}"));
                let _ = tx.send((key, res));
                ctx.request_repaint();
            });
        }
        Tex::Loading
    }

    /// Forget textures of a file (after it changed on disk).
    pub fn invalidate(&mut self, path: &Path) {
        self.textures.retain(|k, _| k.0 != path);
        self.failed.retain(|k, _| k.0 != path);
    }
}

/// Show a texture scaled to fit `max` (keeping aspect), returning the response and rect.
pub fn fitted_image(ui: &mut egui::Ui, tex: &TextureHandle, max: Vec2, sense: Sense) -> (egui::Response, Rect) {
    let size = tex.size_vec2();
    let scale = (max.x / size.x).min(max.y / size.y).min(4.0);
    let draw = size * scale;
    let (rect, resp) = ui.allocate_exact_size(draw, sense);
    ui.painter().image(tex.id(), rect, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), Color32::WHITE);
    (resp, rect)
}

/// Overlay showing which parts of a page are ignored / cropped.
pub fn page_overlay(painter: &egui::Painter, rect: Rect, crop: Option<[f32; 4]>, ignore_top: bool, ignore_bottom: bool) {
    let shade = Color32::from_black_alpha(140);
    if let Some([x0, y0, x1, y1]) = crop {
        let c = Rect::from_min_max(
            Pos2::new(rect.left() + x0.min(x1) * rect.width(), rect.top() + y0.min(y1) * rect.height()),
            Pos2::new(rect.left() + x0.max(x1) * rect.width(), rect.top() + y0.max(y1) * rect.height()),
        );
        painter.rect_filled(Rect::from_min_max(rect.min, Pos2::new(rect.right(), c.top())), 0.0, shade);
        painter.rect_filled(Rect::from_min_max(Pos2::new(rect.left(), c.bottom()), rect.max), 0.0, shade);
        painter.rect_filled(Rect::from_min_max(Pos2::new(rect.left(), c.top()), Pos2::new(c.left(), c.bottom())), 0.0, shade);
        painter.rect_filled(Rect::from_min_max(Pos2::new(c.right(), c.top()), Pos2::new(rect.right(), c.bottom())), 0.0, shade);
        painter.rect_stroke(c, 0.0, Stroke::new(2.0_f32, Color32::from_rgb(80, 170, 255)), StrokeKind::Outside);
    }
    let band = rect.height() * 0.12;
    let label_font = egui::FontId::proportional(13.0);
    if ignore_top {
        let r = Rect::from_min_max(rect.min, Pos2::new(rect.right(), rect.top() + band));
        painter.rect_filled(r, 0.0, Color32::from_rgba_unmultiplied(200, 60, 60, 90));
        painter.text(r.center(), egui::Align2::CENTER_CENTER, "ignoring song at top", label_font.clone(), Color32::WHITE);
    }
    if ignore_bottom {
        let r = Rect::from_min_max(Pos2::new(rect.left(), rect.bottom() - band), rect.max);
        painter.rect_filled(r, 0.0, Color32::from_rgba_unmultiplied(200, 60, 60, 90));
        painter.text(r.center(), egui::Align2::CENTER_CENTER, "ignoring song at bottom", label_font, Color32::WHITE);
    }
}

/// Interactive crop editor. Returns true when the crop changed.
pub fn crop_editor(ui: &mut egui::Ui, tex: &TextureHandle, max: Vec2, crop: &mut Option<[f32; 4]>, drag_start: &mut Option<Pos2>) -> bool {
    let (resp, rect) = fitted_image(ui, tex, max, Sense::click_and_drag());
    let mut changed = false;
    let to_norm = |p: Pos2| -> Pos2 {
        Pos2::new(((p.x - rect.left()) / rect.width()).clamp(0.0, 1.0), ((p.y - rect.top()) / rect.height()).clamp(0.0, 1.0))
    };
    if resp.drag_started() {
        // Start where the button went down, not where egui noticed the drag.
        if let Some(p) = ui.input(|i| i.pointer.press_origin()).or_else(|| resp.interact_pointer_pos()) {
            *drag_start = Some(to_norm(p));
        }
    }
    if resp.dragged() {
        if let (Some(s), Some(p)) = (*drag_start, resp.interact_pointer_pos()) {
            let e = to_norm(p);
            *crop = Some([s.x.min(e.x), s.y.min(e.y), s.x.max(e.x), s.y.max(e.y)]);
            changed = true;
        }
    }
    if resp.drag_stopped() {
        *drag_start = None;
        if let Some([x0, y0, x1, y1]) = *crop {
            if (x1 - x0) < 0.03 || (y1 - y0) < 0.03 {
                *crop = None;
            }
        }
        changed = true;
    }
    page_overlay(ui.painter(), rect, *crop, false, false);
    changed
}

const VOICE_COLORS: [Color32; 8] = [
    Color32::from_rgb(235, 87, 87),
    Color32::from_rgb(242, 153, 74),
    Color32::from_rgb(39, 174, 96),
    Color32::from_rgb(47, 128, 237),
    Color32::from_rgb(155, 81, 224),
    Color32::from_rgb(86, 204, 242),
    Color32::from_rgb(242, 201, 76),
    Color32::from_rgb(130, 130, 130),
];

pub fn voice_color(i: usize) -> Color32 {
    VOICE_COLORS[i % VOICE_COLORS.len()]
}

/// A piano-roll view of the whole song; click to select a measure.
/// Returns the clicked measure index.
pub fn piano_roll(ui: &mut egui::Ui, song: &ParsedSong, selected: Option<usize>, height: f32) -> Option<usize> {
    if song.measures.is_empty() {
        return None;
    }
    let total: u32 = song.measures.iter().map(|m| m.actual_ticks.max(1)).sum();
    let mut lo = 127;
    let mut hi = 0;
    for m in &song.measures {
        for v in &m.voices {
            for e in &v.events {
                for p in e.pitches() {
                    lo = lo.min(p.midi());
                    hi = hi.max(p.midi());
                }
            }
        }
    }
    if lo > hi {
        lo = 48;
        hi = 72;
    }
    lo -= 2;
    hi += 2;
    let width = ui.available_width().max(200.0);
    let px_per_tick = (width / total as f32).max(0.02);
    let content_w = (total as f32 * px_per_tick).max(width);
    let mut clicked = None;
    egui::ScrollArea::horizontal().id_salt("piano_roll").show(ui, |ui| {
        let (rect, resp) = ui.allocate_exact_size(Vec2::new(content_w, height), Sense::click());
        let painter = ui.painter_at(rect);
        let visuals = ui.visuals();
        painter.rect_filled(rect, 4.0, visuals.extreme_bg_color);
        let pitch_h = rect.height() / (hi - lo + 1) as f32;
        let y_of = |midi: i32| rect.bottom() - (midi - lo + 1) as f32 * pitch_h;
        // C lines
        for midi in lo..=hi {
            if midi % 12 == 0 {
                let y = y_of(midi) + pitch_h;
                painter
                    .line_segment([Pos2::new(rect.left(), y), Pos2::new(rect.right(), y)], Stroke::new(0.5_f32, visuals.weak_text_color()));
            }
        }
        let mut x = rect.left();
        let mut starts = Vec::new();
        for m in &song.measures {
            let w = m.actual_ticks.max(1) as f32 * px_per_tick;
            starts.push((x, w));
            if Some(m.index) == selected {
                painter.rect_filled(
                    Rect::from_min_size(Pos2::new(x, rect.top()), Vec2::new(w, rect.height())),
                    0.0,
                    visuals.selection.bg_fill.gamma_multiply(0.35),
                );
            }
            painter.line_segment([Pos2::new(x, rect.top()), Pos2::new(x, rect.bottom())], Stroke::new(0.7_f32, visuals.weak_text_color()));
            painter.text(
                Pos2::new(x + 2.0, rect.top() + 1.0),
                egui::Align2::LEFT_TOP,
                m.number.to_string(),
                egui::FontId::monospace(9.0),
                visuals.weak_text_color(),
            );
            for v in &m.voices {
                let vi = song.voice_index(&v.id).unwrap_or(0);
                let color = voice_color(vi);
                let mut t = 0u32;
                for e in &v.events {
                    let ex = x + t as f32 * px_per_tick;
                    let ew = (e.ticks as f32 * px_per_tick - 1.0).max(1.5);
                    for p in e.pitches() {
                        let y = y_of(p.midi());
                        painter.rect_filled(Rect::from_min_size(Pos2::new(ex, y), Vec2::new(ew, pitch_h.max(2.0))), 1.5, color);
                    }
                    if e.fermata {
                        painter.text(
                            Pos2::new(ex, rect.top() + 10.0),
                            egui::Align2::LEFT_TOP,
                            "𝄐",
                            egui::FontId::proportional(10.0),
                            visuals.text_color(),
                        );
                    }
                    t += e.ticks;
                }
            }
            x += w;
        }
        if resp.clicked() {
            if let Some(p) = resp.interact_pointer_pos() {
                for (i, (sx, w)) in starts.iter().enumerate() {
                    if p.x >= *sx && p.x < sx + w {
                        clicked = Some(i);
                    }
                }
            }
        }
    });
    clicked
}

/// Legend for voice colors.
pub fn voice_legend(ui: &mut egui::Ui, song: &ParsedSong) {
    ui.horizontal_wrapped(|ui| {
        for (i, v) in song.voices.iter().enumerate() {
            let (r, _) = ui.allocate_exact_size(Vec2::splat(10.0), Sense::hover());
            ui.painter().rect_filled(r, 2.0, voice_color(i));
            ui.label(if v.name.is_empty() { v.id.clone() } else { v.name.clone() });
            ui.add_space(6.0);
        }
    });
}

/// Install fallback fonts with music and symbol glyphs (♯ ♭ ♮ 𝄐) when available.
pub fn install_fonts(ctx: &egui::Context) {
    let candidates: &[&str] = &[
        "C:\\Windows\\Fonts\\seguisym.ttf",
        "C:\\Windows\\Fonts\\seguiemj.ttf",
        "/System/Library/Fonts/Apple Symbols.ttf",
        "/Library/Fonts/Arial Unicode.ttf",
        "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
        "/usr/share/fonts/TTF/DejaVuSans.ttf",
        "/usr/share/fonts/truetype/noto/NotoSansSymbols2-Regular.ttf",
        "/usr/share/fonts/truetype/noto/NotoMusic-Regular.ttf",
    ];
    let mut fonts = egui::FontDefinitions::default();
    let mut added = false;
    for (i, path) in candidates.iter().enumerate() {
        if let Ok(bytes) = std::fs::read(path) {
            let name = format!("fallback{i}");
            fonts.font_data.insert(name.clone(), std::sync::Arc::new(egui::FontData::from_owned(bytes)));
            for fam in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
                fonts.families.entry(fam).or_default().push(name.clone());
            }
            added = true;
        }
    }
    if added {
        ctx.set_fonts(fonts);
    }
}
