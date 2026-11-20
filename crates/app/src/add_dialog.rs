//! The window shown before a torrent added from the app starts: where to save it, its
//! category, which files to download, and whether to start paused.
use std::collections::VecDeque;
use std::path::PathBuf;

use eframe::egui::{self, RichText};
use rusttorrent_core::{format, manage, AddOptions, Command, Core, Inspection, Snapshot};

/// The choices for the torrent on show.
struct Form {
    text: String,
    folder: Option<PathBuf>,
    category: Option<String>,
    /// The name typed for a new category, if any.
    new_category: String,
    paused: bool,
    /// One per file once the list is known. Empty until then.
    files: Vec<bool>,
}

impl Form {
    fn new(text: String) -> Self {
        Self {
            text,
            folder: None,
            category: None,
            new_category: String::new(),
            paused: false,
            files: Vec::new(),
        }
    }
}

/// Torrents waiting to be added, shown one at a time.
#[derive(Default)]
pub struct AddQueue {
    current: Option<Form>,
    waiting: VecDeque<String>,
    /// What the core was last asked to fetch details for.
    asked: Vec<String>,
    /// The snapshot the last request was checked against.
    checked: Option<std::sync::Arc<Snapshot>>,
}

impl AddQueue {
    pub fn push(&mut self, text: String) {
        let text = text.trim().to_owned();
        if text.is_empty() || self.current.as_ref().is_some_and(|f| f.text == text) || self.waiting.contains(&text) {
            return;
        }
        if self.current.is_none() {
            self.current = Some(Form::new(text));
        } else {
            self.waiting.push_back(text);
        }
    }

    pub fn is_open(&self) -> bool {
        self.current.is_some()
    }

    fn next(&mut self) {
        self.current = self.waiting.pop_front().map(Form::new);
    }

    /// Keeps the core fetching details for every torrent waiting here, and nothing else.
    fn ask(&mut self, core: &Core, snap: &std::sync::Arc<Snapshot>) {
        let wanted: Vec<String> = self
            .current
            .iter()
            .map(|f| f.text.clone())
            .chain(self.waiting.iter().cloned())
            .collect();
        let changed = wanted != self.asked;
        // The engine forgets what it fetched when it stops, so ask again if it did.
        let fresh = self.checked.as_ref().is_none_or(|s| !std::sync::Arc::ptr_eq(s, snap));
        let missing = fresh && wanted.iter().any(|t| !snap.inspected.iter().any(|i| &i.text == t));
        if changed || missing {
            core.send(Command::Inspect(wanted.clone()));
            self.asked = wanted;
        }
        self.checked = Some(snap.clone());
    }

    pub fn show(&mut self, ctx: &egui::Context, core: &Core, snap: &std::sync::Arc<Snapshot>, categories: &[String]) {
        self.ask(core, snap);
        let more = self.waiting.len();
        let Some(form) = &mut self.current else { return };
        let found: Option<&Inspection> = snap.inspected.iter().find(|i| i.text == form.text);
        if let Some(found) = found.filter(|f| f.ready && f.error.is_none()) {
            if form.files.len() != found.files.len() {
                form.files = vec![true; found.files.len()];
            }
        }
        let mut categories: Vec<String> = categories.to_vec();
        for name in snap.settings.category_dirs.keys() {
            if !categories.contains(name) {
                categories.push(name.clone());
            }
        }
        let (mut add, mut cancel) = (false, false);
        let response = egui::Modal::new(egui::Id::unique("add-choices")).show(ctx, |ui| {
            ui.set_width(520.0);
            ui.heading(if more > 0 {
                format!("Add torrent (1 of {})", more + 1)
            } else {
                "Add torrent".to_owned()
            });
            ui.add_space(4.0);
            let name = found.map(|f| f.name.clone()).unwrap_or_else(|| form.text.clone());
            ui.add(egui::Label::new(RichText::new(name).strong()).truncate());
            ui.add_space(6.0);
            egui::Grid::new("add-grid")
                .num_columns(2)
                .spacing([12.0, 6.0])
                .show(ui, |ui| {
                    ui.label("Category");
                    ui.horizontal(|ui| {
                        let shown = form.category.clone().unwrap_or_else(|| "None".to_owned());
                        let before = form.category.clone();
                        egui::ComboBox::from_id_salt("add-category")
                            .selected_text(shown)
                            .width(180.0)
                            .show_ui(ui, |ui| {
                                ui.selectable_value(&mut form.category, None, "None");
                                for name in &categories {
                                    ui.selectable_value(&mut form.category, Some(name.clone()), name);
                                }
                            });
                        if form.category != before {
                            form.new_category.clear();
                        }
                        let typed = ui.add(
                            egui::TextEdit::singleline(&mut form.new_category)
                                .hint_text("or type a new one")
                                .desired_width(160.0),
                        );
                        if typed.changed() {
                            let name = form.new_category.trim();
                            form.category = (!name.is_empty()).then(|| name.to_owned());
                        }
                    });
                    ui.end_row();
                    ui.label("Save to");
                    ui.horizontal(|ui| {
                        let start = manage::folder_for(&snap.settings, form.folder.as_ref(), form.category.as_deref());
                        if let Some(dir) = crate::folders::button(ui, "Change", &snap.settings.recent_dirs, &start) {
                            form.folder = Some(dir);
                        }
                        if form.folder.is_some() && ui.button("Default").clicked() {
                            form.folder = None;
                        }
                        let folder = manage::folder_for(&snap.settings, form.folder.as_ref(), form.category.as_deref());
                        ui.add(egui::Label::new(folder.display().to_string()).truncate());
                    });
                    ui.end_row();
                });
            ui.add_space(4.0);
            ui.checkbox(&mut form.paused, "Start paused");
            ui.add_space(8.0);
            match found {
                Some(f) if f.ready && f.error.is_some() => {
                    let error = f.error.clone().unwrap_or_default();
                    ui.label(RichText::new(error).small().color(ui.visuals().error_fg_color));
                    ui.label(
                        RichText::new("It can still be added. All files will be downloaded.")
                            .small()
                            .weak(),
                    );
                }
                Some(f) if f.ready => {
                    let total: u64 = f.files.iter().map(|m| m.len).sum();
                    let chosen: u64 = f
                        .files
                        .iter()
                        .zip(&form.files)
                        .filter(|(_, on)| **on)
                        .map(|(m, _)| m.len)
                        .sum();
                    ui.horizontal(|ui| {
                        ui.label(format!(
                            "Files: {} of {} chosen",
                            format::bytes(chosen),
                            format::bytes(total)
                        ));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.small_button("None").clicked() {
                                form.files.iter_mut().for_each(|on| *on = false);
                            }
                            if ui.small_button("All").clicked() {
                                form.files.iter_mut().for_each(|on| *on = true);
                            }
                        });
                    });
                    let row = ui.text_style_height(&egui::TextStyle::Body) + 4.0;
                    egui::ScrollArea::vertical()
                        .max_height(220.0)
                        .auto_shrink([false, true])
                        .show_rows(ui, row, f.files.len(), |ui, range| {
                            for i in range {
                                let (Some(meta), Some(on)) = (f.files.get(i), form.files.get_mut(i)) else {
                                    continue;
                                };
                                ui.horizontal(|ui| {
                                    ui.checkbox(on, "");
                                    ui.label(RichText::new(format::bytes(meta.len)).weak());
                                    ui.add(egui::Label::new(&meta.name).truncate());
                                });
                            }
                        });
                }
                _ => {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("Fetching the file list…");
                    });
                    ui.label(
                        RichText::new("You can add it now. Then all files will be downloaded.")
                            .small()
                            .weak(),
                    );
                }
            }
            ui.add_space(8.0);
            let none_chosen = !form.files.is_empty() && !form.files.iter().any(|on| *on);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let button = egui::Button::new(RichText::new("Add").strong());
                if ui
                    .add_enabled(!none_chosen, button)
                    .on_disabled_hover_text("Choose at least one file.")
                    .clicked()
                {
                    add = true;
                }
                if ui.button("Cancel").clicked() {
                    cancel = true;
                }
            });
        });
        if add {
            let files = if form.files.iter().all(|on| *on) {
                None
            } else {
                Some(
                    form.files
                        .iter()
                        .enumerate()
                        .filter(|(_, on)| **on)
                        .map(|(i, _)| i)
                        .collect(),
                )
            };
            core.send(Command::AddWith(AddOptions {
                text: form.text.clone(),
                folder: form.folder.clone(),
                category: form.category.clone(),
                paused: form.paused,
                files,
            }));
        }
        if add || cancel || response.should_close() {
            self.next();
            self.ask(core, snap);
        }
    }
}
