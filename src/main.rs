#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use eframe::egui;
use sheet_music_convertor::ui::App;

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Sheet Music Convertor")
            .with_inner_size([1400.0, 900.0])
            .with_min_inner_size([900.0, 600.0])
            .with_drag_and_drop(true),
        ..Default::default()
    };
    eframe::run_native("Sheet Music Convertor", options, Box::new(|cc| Ok(Box::new(App::new(cc)))))
}
