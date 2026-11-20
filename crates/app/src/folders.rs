//! A drop-down for choosing a folder: the folders chosen lately, then "Other folder…",
//! which opens the system's folder picker.
use std::path::{Path, PathBuf};

use eframe::egui;

/// Fills an open menu with the recent folders (those that still exist) and "Other folder…".
/// Returns the folder picked, if any. `start` is where the folder picker opens.
pub fn menu_items(ui: &mut egui::Ui, recent: &[PathBuf], start: &Path) -> Option<PathBuf> {
    let mut picked = None;
    let mut any = false;
    for dir in recent.iter().filter(|d| d.is_dir()) {
        any = true;
        let name = dir.file_name().map(|n| n.to_string_lossy().into_owned());
        let label = name.unwrap_or_else(|| dir.display().to_string());
        if ui.button(label).on_hover_text(dir.display().to_string()).clicked() {
            picked = Some(dir.clone());
            ui.close();
        }
    }
    if any {
        ui.separator();
    }
    if ui.button("Other folder…").clicked() {
        ui.close();
        picked = rfd::FileDialog::new().set_directory(start).pick_folder();
    }
    picked
}

/// A "Change ⏷" button that opens [`menu_items`] below it.
pub fn button(ui: &mut egui::Ui, text: &str, recent: &[PathBuf], start: &Path) -> Option<PathBuf> {
    ui.menu_button(format!("{text} ⏷"), |ui| menu_items(ui, recent, start))
        .inner
        .flatten()
}
