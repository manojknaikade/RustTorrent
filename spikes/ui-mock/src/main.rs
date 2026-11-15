//! Interface mock-up with fake data: 5,000 torrents, names in several scripts.
//! Nothing here talks to the network. It exists so the layout can be judged and measured.
use eframe::egui::{self, Color32, RichText, Sense};
use egui_extras::{Column, TableBuilder};

#[derive(Clone, Copy, PartialEq)]
enum State { Downloading, Seeding, Paused, Queued, Done, Error }

impl State {
    fn label(self) -> &'static str {
        match self {
            State::Downloading => "Downloading", State::Seeding => "Seeding", State::Paused => "Paused",
            State::Queued => "Queued", State::Done => "Completed", State::Error => "Error",
        }
    }
    fn color(self) -> Color32 {
        match self {
            State::Downloading => Color32::from_rgb(64, 156, 255), State::Seeding => Color32::from_rgb(52, 199, 123),
            State::Paused => Color32::from_rgb(150, 150, 158), State::Queued => Color32::from_rgb(230, 170, 60),
            State::Done => Color32::from_rgb(120, 200, 190), State::Error => Color32::from_rgb(240, 90, 90),
        }
    }
}

struct Torrent {
    name: String, state: State, progress: f32, size: u64, down: u64, up: u64,
    peers: u32, seeds: u32, ratio: f32, category: &'static str, in_order: bool,
}

const NAMES: &[&str] = &[
    "debian-13.7.0-arm64-netinst.iso", "ubuntu-26.04-desktop-arm64.iso", "Big Buck Bunny (2008) 4K 60fps",
    "アーカイブ 公開講義 第12回 量子力学入門", "公共领域电影合集 第三卷 高清修复版", "सार्वजनिक डोमेन संगीत संग्रह भाग २",
    "Архив открытых лекций — математика, том 4", "أرشيف الكتب المفتوحة - المجلد الخامس", "한국어 오픈 강의 모음집 2026",
    "Sintel.2010.2160p.open-movie", "archlinux-2026.11.01-x86_64.iso", "Tears of Steel — Blender Foundation",
    "LibriVox Collection: 19th Century Novels (very long name to test how the row cuts text off at the edge)",
    "OpenStreetMap planet-261101.osm.pbf", "Wikipedia EN snapshot 2026-10 (text only)",
];
const CATS: &[&str] = &["Linux", "Films", "Lectures", "Music", "Data"];

fn fake(n: usize) -> Vec<Torrent> {
    let mut seed = 0x2545F4914F6CDD1Du64;
    let mut r = move || { seed ^= seed << 13; seed ^= seed >> 7; seed ^= seed << 17; seed };
    (0..n).map(|i| {
        let state = match r() % 20 { 0..=4 => State::Downloading, 5..=10 => State::Seeding, 11..=13 => State::Paused,
            14..=15 => State::Queued, 16..=18 => State::Done, _ => State::Error };
        let progress = match state { State::Seeding | State::Done => 1.0, State::Queued => 0.0, _ => (r() % 1000) as f32 / 1000.0 };
        Torrent {
            name: if i < NAMES.len() { NAMES[i].to_string() } else { format!("{} [{}]", NAMES[(r() % NAMES.len() as u64) as usize], i) },
            state, progress, size: 50_000_000 + r() % 60_000_000_000,
            down: if state == State::Downloading { r() % 40_000_000 } else { 0 },
            up: if matches!(state, State::Downloading | State::Seeding) { r() % 3_000_000 } else { 0 },
            peers: (r() % 80) as u32, seeds: (r() % 400) as u32, ratio: (r() % 400) as f32 / 100.0,
            category: CATS[(r() % CATS.len() as u64) as usize], in_order: r() % 3 == 0,
        }
    }).collect()
}

fn bytes(b: u64) -> String {
    const U: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let (mut v, mut i) = (b as f64, 0);
    while v >= 1000.0 && i < 4 { v /= 1000.0; i += 1; }
    if i == 0 { format!("{b} B") } else { format!("{v:.1} {}", U[i]) }
}
fn speed(b: u64) -> String { if b == 0 { "–".into() } else { format!("{}/s", bytes(b)) } }
fn eta(t: &Torrent) -> String {
    if t.down == 0 || t.progress >= 1.0 { return "–".into(); }
    let s = ((t.size as f64) * (1.0 - t.progress as f64) / t.down as f64) as u64;
    if s >= 3600 { format!("{}h {}m", s / 3600, s % 3600 / 60) } else { format!("{}m {}s", s / 60, s % 60) }
}

#[derive(PartialEq, Clone, Copy)]
enum Tab { Overview, Files, Peers, Trackers }

struct App {
    all: Vec<Torrent>, view: Vec<usize>, selected: Option<usize>, filter: Option<State>, category: Option<&'static str>,
    search: String, sort: (usize, bool), tab: Tab, dirty: bool,
}

impl App {
    fn new() -> Self {
        Self { all: fake(5000), view: vec![], selected: Some(0), filter: None, category: None,
               search: String::new(), sort: (0, true), tab: Tab::Overview, dirty: true }
    }
    fn rebuild(&mut self) {
        let q = self.search.to_lowercase();
        let all = &self.all;
        self.view = (0..all.len()).filter(|&i| {
            let t = &all[i];
            self.filter.map_or(true, |f| t.state == f) && self.category.map_or(true, |c| t.category == c)
                && (q.is_empty() || t.name.to_lowercase().contains(&q))
        }).collect();
        let (col, asc) = self.sort;
        if col > 0 {
            self.view.sort_by(|&a, &b| {
                let (a, b) = (&all[a], &all[b]);
                let o = match col {
                    1 => a.name.cmp(&b.name), 2 => a.progress.total_cmp(&b.progress), 3 => a.size.cmp(&b.size),
                    4 => a.down.cmp(&b.down), 5 => a.up.cmp(&b.up), _ => a.ratio.total_cmp(&b.ratio),
                };
                if asc { o } else { o.reverse() }
            });
        }
        self.dirty = false;
    }
}

fn bar(ui: &mut egui::Ui, t: &Torrent) {
    ui.add(egui::ProgressBar::new(t.progress).fill(t.state.color().gamma_multiply(0.75))
        .text(RichText::new(format!("{:.1}%", t.progress * 100.0)).size(11.0)).desired_height(14.0));
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        if self.dirty { self.rebuild(); }
        // `MOCK_SHOT=<file.png>` saves a picture of the window after it has settled.
        if let Ok(path) = std::env::var("MOCK_SHOT") {
            let ctx = ui.ctx().clone();
            let n = ctx.cumulative_pass_nr();
            if n < 60 { ctx.request_repaint(); }
            if n == 40 {
                ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::ScreenshotCallback::new(move |img| {
                    let f = std::io::BufWriter::new(std::fs::File::create(&path).unwrap());
                    let mut enc = png::Encoder::new(f, img.width() as u32, img.height() as u32);
                    enc.set_color(png::ColorType::Rgba);
                    enc.write_header().unwrap().write_image_data(img.as_raw()).unwrap();
                })));
            }
        }
        let (down, up): (u64, u64) = self.all.iter().fold((0, 0), |a, t| (a.0 + t.down, a.1 + t.up));

        egui::Panel::top("bar").show(ui, |ui| {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                let _ = ui.button(RichText::new("+ Add torrent").strong());
                let _ = ui.button("Pause");
                let _ = ui.button("Resume");
                let _ = ui.button("Remove");
                ui.separator();
                if ui.add(egui::TextEdit::singleline(&mut self.search).hint_text("Search names").desired_width(260.0)).changed() {
                    self.dirty = true;
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(format!("↑ {}", speed(up)));
                    ui.label(RichText::new(format!("↓ {}", speed(down))).strong());
                    ui.separator();
                    ui.label(RichText::new("● en0 (bound)").color(State::Seeding.color()));
                });
            });
            ui.add_space(6.0);
        });

        egui::Panel::left("side").resizable(false).exact_size(170.0).show(ui, |ui| {
            ui.add_space(8.0);
            ui.label(RichText::new("STATUS").small().weak());
            let count = |f: Option<State>| self.all.iter().filter(|t| f.map_or(true, |f| t.state == f)).count();
            for f in [None, Some(State::Downloading), Some(State::Seeding), Some(State::Queued), Some(State::Paused), Some(State::Done), Some(State::Error)] {
                let name = f.map_or("All", |s| s.label());
                let text = format!("{name}  {}", count(f));
                let rt = match f { Some(s) => RichText::new(format!("● {text}")).color(if self.filter == f { Color32::WHITE } else { s.color() }), None => RichText::new(text) };
                if ui.selectable_label(self.filter == f, rt).clicked() { self.filter = f; self.dirty = true; }
            }
            ui.add_space(12.0);
            ui.label(RichText::new("CATEGORIES").small().weak());
            if ui.selectable_label(self.category.is_none(), "All").clicked() { self.category = None; self.dirty = true; }
            for c in CATS {
                if ui.selectable_label(self.category == Some(*c), *c).clicked() { self.category = Some(*c); self.dirty = true; }
            }
        });

        // Details live in the main window, always visible for the selected torrent.
        egui::Panel::bottom("details").resizable(true).default_size(230.0).min_size(140.0).show(ui, |ui| {
            let Some(t) = self.selected.and_then(|i| self.all.get_mut(i)) else {
                ui.centered_and_justified(|ui| { ui.label("Select a torrent to see its details"); });
                return;
            };
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new(&t.name).strong().size(15.0));
                ui.label(RichText::new(t.state.label()).color(t.state.color()));
            });
            ui.horizontal(|ui| {
                for (tab, name) in [(Tab::Overview, "Overview"), (Tab::Files, "Files"), (Tab::Peers, "Peers"), (Tab::Trackers, "Trackers")] {
                    ui.selectable_value(&mut self.tab, tab, name);
                }
            });
            ui.separator();
            match self.tab {
                Tab::Overview => {
                    // Piece map: which parts of the torrent are on disk. In-order torrents fill from the left.
                    let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 18.0), Sense::hover());
                    let p = ui.painter();
                    p.rect_filled(rect, 3.0, ui.visuals().extreme_bg_color);
                    let n = 240;
                    let w = rect.width() / n as f32;
                    for k in 0..n {
                        let have = if t.in_order || t.progress >= 1.0 { (k as f32 / n as f32) < t.progress }
                                   else { ((k * 2654435761usize) % 1000) as f32 / 1000.0 < t.progress };
                        if have {
                            let x = rect.left() + k as f32 * w;
                            p.rect_filled(egui::Rect::from_min_size(egui::pos2(x, rect.top()), egui::vec2(w + 0.5, rect.height())), 0.0, t.state.color().gamma_multiply(0.8));
                        }
                    }
                    ui.add_space(6.0);
                    ui.checkbox(&mut t.in_order, "Download in order (watch while it downloads)");
                    ui.add_space(4.0);
                    egui::Grid::new("ov").num_columns(6).spacing([28.0, 6.0]).show(ui, |ui| {
                        let done = (t.size as f64 * t.progress as f64) as u64;
                        for (k, v) in [("Downloaded", format!("{} of {}", bytes(done), bytes(t.size))), ("Download", speed(t.down)), ("Upload", speed(t.up))] {
                            ui.label(RichText::new(k).weak()); ui.label(v);
                        }
                        ui.end_row();
                        for (k, v) in [("Time left", eta(t)), ("Peers", format!("{} connected, {} seeds", t.peers, t.seeds)), ("Ratio", format!("{:.2} (stops at 2.00)", t.ratio))] {
                            ui.label(RichText::new(k).weak()); ui.label(v);
                        }
                        ui.end_row();
                        ui.label(RichText::new("Category").weak()); ui.label(t.category);
                        ui.label(RichText::new("Saved to").weak()); ui.label(format!("~/Downloads/{}", t.category));
                        ui.end_row();
                    });
                }
                Tab::Files => {
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        for k in 0..8 {
                            ui.horizontal(|ui| {
                                let mut on = k != 5;
                                ui.checkbox(&mut on, format!("part-{:02}.mkv", k + 1));
                                ui.label(RichText::new(bytes(t.size / 8)).weak());
                                ui.add(egui::ProgressBar::new((t.progress * 8.0 - k as f32).clamp(0.0, 1.0)).desired_width(160.0).desired_height(10.0));
                            });
                        }
                    });
                }
                Tab::Peers => {
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        for k in 0..t.peers.min(30) {
                            ui.label(format!("203.0.113.{:<3}   ↓ {:<12} ↑ {}", k + 7, speed(t.down / (k as u64 + 2)), speed(t.up / (k as u64 + 2))));
                        }
                    });
                }
                Tab::Trackers => { ui.label("udp://tracker.example.org:1337   (per-tracker status is not available from the engine)"); }
            }
        });

        egui::CentralPanel::default().show(ui, |ui| {
            let heads = ["", "Name", "Progress", "Size", "Down", "Up", "Ratio"];
            let (view, all) = (&self.view, &self.all);
            let (mut sel, mut sort) = (self.selected, self.sort);
            TableBuilder::new(ui).striped(true).sense(Sense::click())
                .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                .column(Column::exact(14.0)).column(Column::remainder().at_least(200.0).clip(true))
                .column(Column::exact(150.0)).column(Column::exact(80.0)).column(Column::exact(90.0))
                .column(Column::exact(90.0)).column(Column::exact(60.0))
                .header(24.0, |mut h| {
                    for (i, name) in heads.iter().enumerate() {
                        h.col(|ui| {
                            let mark = if sort.0 == i && i > 0 { if sort.1 { " ▲" } else { " ▼" } } else { "" };
                            if ui.add(egui::Label::new(RichText::new(format!("{name}{mark}")).strong()).sense(Sense::click())).clicked() && i > 0 {
                                sort = if sort.0 == i { (i, !sort.1) } else { (i, i == 1) };
                            }
                        });
                    }
                })
                .body(|body| {
                    body.rows(26.0, view.len(), |mut row| {
                        let idx = view[row.index()];
                        let t = &all[idx];
                        row.set_selected(sel == Some(idx));
                        row.col(|ui| { ui.label(RichText::new("●").color(t.state.color())); });
                        row.col(|ui| { ui.add(egui::Label::new(&t.name).truncate()); });
                        row.col(|ui| bar(ui, t));
                        row.col(|ui| { ui.label(bytes(t.size)); });
                        row.col(|ui| { ui.label(speed(t.down)); });
                        row.col(|ui| { ui.label(speed(t.up)); });
                        row.col(|ui| { ui.label(format!("{:.2}", t.ratio)); });
                        if row.response().clicked() { sel = Some(idx); }
                    });
                });
            if sort != self.sort { self.sort = sort; self.dirty = true; }
            self.selected = sel;
        });
    }
}

fn main() -> eframe::Result {
    let opts = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([1180.0, 760.0]).with_title("RustTorrent — mock-up"),
        ..Default::default()
    };
    eframe::run_native("RustTorrent mock-up", opts, Box::new(|_| Ok(Box::new(App::new()))))
}
