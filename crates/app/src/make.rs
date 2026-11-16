//! The "Create torrent" window. The work itself is done by the core, which reports its
//! progress in the snapshot.
use std::path::{Path, PathBuf};

use eframe::egui::{self, RichText};
use rusttorrent_core::make::{date_text, MAX_PIECE, MIN_PIECE};
use rusttorrent_core::{format, Command, Core, Details, MakeOptions, Snapshot};

/// The Overview line for what a torrent's file says about itself.
pub fn detail_row(d: &Details) -> [(&'static str, String); 3] {
    let pieces = format!("{} × {}", d.pieces, piece_text(d.piece_len));
    [
        ("Pieces", pieces),
        ("Created by", d.created_by.clone().unwrap_or_else(|| "–".into())),
        ("Created on", d.created_at.map_or("–".into(), date_text)),
    ]
}

/// Piece sizes are powers of two, so they read best in KiB and MiB ("16 KiB", not "16.4 KB").
fn piece_text(len: u32) -> String {
    match len {
        l if l >= 1 << 20 && l % (1 << 20) == 0 => format!("{} MiB", l >> 20),
        l if l % 1024 == 0 => format!("{} KiB", l >> 10),
        l => format::bytes(l as u64),
    }
}

pub struct CreateDialog {
    source: Option<PathBuf>,
    trackers: String,
    /// `None` chooses one from the size.
    piece: Option<u32>,
    private: bool,
    comment: String,
    save_to: Option<PathBuf>,
    seed: bool,
    /// While working: the number of the last job before this one.
    after: Option<u64>,
    /// Why the last try did not work.
    error: Option<String>,
}

impl CreateDialog {
    pub fn new() -> Self {
        Self {
            source: None,
            trackers: String::new(),
            piece: None,
            private: false,
            comment: String::new(),
            save_to: None,
            seed: true,
            after: None,
            error: None,
        }
    }

    /// Testing only: starts with this file or folder chosen.
    #[cfg(feature = "shot")]
    pub fn with_source(path: PathBuf) -> Self {
        let mut d = Self::new();
        d.set_source(path);
        d
    }

    fn set_source(&mut self, path: PathBuf) {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let dir = path.parent().map(Path::to_path_buf).unwrap_or_default();
        self.save_to = Some(dir.join(format!("{name}.torrent")));
        self.source = Some(path);
        self.error = None;
    }

    pub fn start(&mut self, core: &Core, snap: &Snapshot) {
        let (Some(source), Some(save_to)) = (self.source.clone(), self.save_to.clone()) else {
            return;
        };
        self.after = Some(snap.making.as_ref().map_or(0, |m| m.job));
        self.error = None;
        core.send(Command::CreateTorrent(MakeOptions {
            source,
            trackers: self.trackers.clone(),
            piece_len: self.piece,
            private: self.private,
            comment: self.comment.clone(),
            save_to,
            seed: self.seed,
        }));
    }

    /// Draws the window. Returns true when it should close.
    pub fn show(&mut self, ctx: &egui::Context, core: &Core, snap: &Snapshot) -> bool {
        // The job this window started, once the core has taken it up.
        let job = match (self.after, &snap.making) {
            (Some(after), Some(m)) if m.job > after => Some(m),
            _ => None,
        };
        if let Some(m) = job {
            match &m.result {
                Some(Ok(_)) => return true,
                Some(Err(e)) => {
                    self.after = None;
                    self.error = (e != "cancelled").then(|| e.clone());
                }
                None => {}
            }
        }
        let working = self.after.is_some();
        let mut close = false;
        let response = egui::Modal::new(egui::Id::unique("create")).show(ctx, |ui| {
            ui.set_width(520.0);
            ui.heading("Create torrent");
            ui.add_space(4.0);
            ui.add_enabled_ui(!working, |ui| self.form(ui));
            ui.add_space(8.0);
            if working {
                let (done, total, progress) = job.map_or((0, 0, 0.0), |m| (m.done, m.total, m.progress()));
                let text = if total == 0 {
                    "Reading the files…".to_owned()
                } else {
                    format!("{} of {}", format::bytes(done), format::bytes(total))
                };
                ui.add(egui::ProgressBar::new(progress).text(text).desired_height(18.0));
                ui.add_space(6.0);
            }
            if let Some(e) = &self.error {
                ui.label(
                    RichText::new(format!("Could not create it: {e}")).color(egui::Color32::from_rgb(220, 80, 70)),
                );
                ui.add_space(6.0);
            }
            ui.horizontal(|ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if working {
                        if ui.button("Cancel").clicked() {
                            core.send(Command::CancelCreate);
                        }
                    } else {
                        let ready = self.source.is_some() && self.save_to.is_some();
                        if ui
                            .add_enabled(ready, egui::Button::new(RichText::new("Create").strong()))
                            .clicked()
                        {
                            self.start(core, snap);
                        }
                        if ui.button("Close").clicked() {
                            close = true;
                        }
                    }
                });
            });
        });
        if working {
            // Progress arrives once a second; keep the bar moving without input.
            ctx.request_repaint_after(std::time::Duration::from_millis(250));
        }
        close || (!working && response.should_close())
    }

    fn form(&mut self, ui: &mut egui::Ui) {
        egui::Grid::new("create-form")
            .num_columns(2)
            .spacing([12.0, 8.0])
            .show(ui, |ui| {
                ui.label("Share");
                ui.horizontal(|ui| {
                    if ui.button("Choose file…").clicked() {
                        if let Some(path) = rfd::FileDialog::new().pick_file() {
                            self.set_source(path);
                        }
                    }
                    if ui.button("Choose folder…").clicked() {
                        if let Some(path) = rfd::FileDialog::new().pick_folder() {
                            self.set_source(path);
                        }
                    }
                });
                ui.end_row();
                ui.label("");
                match &self.source {
                    Some(p) => ui.add(egui::Label::new(p.display().to_string()).truncate()),
                    None => ui.label(RichText::new("Nothing chosen yet.").weak()),
                };
                ui.end_row();

                ui.label("Trackers");
                ui.add(
                    egui::TextEdit::multiline(&mut self.trackers)
                        .desired_rows(3)
                        .desired_width(f32::INFINITY)
                        .hint_text("One address per line. An empty line starts a new tier."),
                );
                ui.end_row();

                ui.label("Piece size");
                let label = |p: Option<u32>| p.map_or("Automatic".to_owned(), piece_text);
                egui::ComboBox::from_id_salt("piece")
                    .selected_text(label(self.piece))
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut self.piece, None, label(None));
                        let mut size = MIN_PIECE;
                        while size <= MAX_PIECE {
                            ui.selectable_value(&mut self.piece, Some(size), label(Some(size)));
                            size *= 2;
                        }
                    });
                ui.end_row();

                ui.label("Comment");
                ui.add(egui::TextEdit::singleline(&mut self.comment).desired_width(f32::INFINITY));
                ui.end_row();

                ui.label("Save as");
                ui.horizontal(|ui| {
                    if ui.button("Choose…").clicked() {
                        let mut pick = rfd::FileDialog::new().add_filter("Torrent files", &["torrent"]);
                        if let Some(p) = &self.save_to {
                            if let Some(dir) = p.parent() {
                                pick = pick.set_directory(dir);
                            }
                            if let Some(name) = p.file_name() {
                                pick = pick.set_file_name(name.to_string_lossy());
                            }
                        }
                        if let Some(path) = pick.save_file() {
                            self.save_to = Some(path);
                        }
                    }
                    match &self.save_to {
                        Some(p) => ui.add(egui::Label::new(p.display().to_string()).truncate()),
                        None => ui.label(RichText::new("–").weak()),
                    };
                });
                ui.end_row();

                ui.label("");
                ui.vertical(|ui| {
                    ui.checkbox(&mut self.private, "Private: only use the trackers to find peers");
                    ui.checkbox(&mut self.seed, "Start seeding it now");
                });
                ui.end_row();
            });
    }
}
