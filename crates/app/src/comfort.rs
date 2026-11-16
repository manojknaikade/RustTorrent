//! Speed graph, piece map, statistics and log windows, the tracker filter, the column
//! chooser, the theme, and what happens when downloads finish. `app.rs` calls in here.
use std::collections::BTreeMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use eframe::egui::{self, Color32, Pos2, Rect, RichText, Sense, Shape, Stroke, Vec2};
use rusttorrent_core::comfort::HISTORY_SECS;
use rusttorrent_core::{
    format, ComfortSettings, PieceMap, Snapshot, SpeedHistory, State, Theme, TorrentView, WhenAllDone,
};

#[cfg(target_os = "macos")]
use crate::mac_extras as sys;

#[cfg(not(target_os = "macos"))]
mod sys {
    pub fn ask_for_notifications() {}
    pub fn notify(_title: &str, _body: &str) {}
    pub fn notification_problem() -> Option<String> {
        None
    }
    #[derive(Default)]
    pub struct Awake;
    impl Awake {
        pub fn hold(&mut self, _on: bool) {}
    }
}

/// Set while the light theme is showing, so colours can be made darker to stay readable.
static LIGHT: AtomicBool = AtomicBool::new(false);

/// Makes a colour darker when the light theme is showing.
pub fn ink(c: Color32) -> Color32 {
    if LIGHT.load(Ordering::Relaxed) {
        Color32::from_rgb(
            (c.r() as f32 * 0.72) as u8,
            (c.g() as f32 * 0.72) as u8,
            (c.b() as f32 * 0.72) as u8,
        )
    } else {
        c
    }
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Window state that is not about one torrent.
#[derive(Default)]
pub struct Extras {
    /// The newest finished download already told about.
    last_finished: u64,
    awake: sys::Awake,
    theme: Option<Theme>,
    asked: bool,
    closing: bool,
    pub stats: bool,
    pub log: Option<LogView>,
}

impl Extras {
    /// Runs before each frame, and also while the window is hidden.
    pub fn logic(&mut self, ctx: &egui::Context, snap: &Snapshot) {
        let c = &snap.settings.comfort;
        let theme = shot_theme().unwrap_or(c.theme);
        if self.theme != Some(theme) {
            ctx.set_theme(match theme {
                Theme::Dark => egui::ThemePreference::Dark,
                Theme::Light => egui::ThemePreference::Light,
                Theme::System => egui::ThemePreference::System,
            });
            self.theme = Some(theme);
        }
        LIGHT.store(ctx.theme() == egui::Theme::Light, Ordering::Relaxed);
        if !snap.ready {
            return;
        }

        if c.notify_finished && !self.asked {
            sys::ask_for_notifications();
            self.asked = true;
        }
        let seen = self.last_finished;
        for f in snap.finished.iter().filter(|f| f.id > seen) {
            if c.notify_finished {
                sys::notify("Download finished", &f.name);
            }
            self.last_finished = self.last_finished.max(f.id);
        }

        let busy = snap.up_bps > 0
            || snap
                .torrents
                .iter()
                .any(|t| matches!(t.state, State::Downloading | State::FetchingInfo | State::Checking));
        self.awake.hold(c.keep_awake && busy);

        if snap.quit_now && !self.closing {
            self.closing = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    /// The statistics and log windows, when open.
    pub fn windows(&mut self, ctx: &egui::Context, snap: &Snapshot) {
        let mut open_log = false;
        let mut stats = self.stats;
        egui::Window::new("Statistics")
            .open(&mut stats)
            .resizable(false)
            .collapsible(false)
            .show(ctx, |ui| {
                ui.label(RichText::new("Speed over the last 5 minutes").weak());
                speed_graph(ui, &snap.speeds, Vec2::new(440.0, 140.0), true);
                ui.horizontal(|ui| {
                    ui.label(RichText::new("↓ download").color(ink(DOWN)));
                    ui.label(RichText::new("↑ upload").color(ink(UP)));
                });
                ui.add_space(8.0);
                ui.label(RichText::new("Since RustTorrent was first started").weak());
                let a = &snap.all_time;
                let running = a.running_secs + now().saturating_sub(snap.running_since);
                egui::Grid::new("all_time")
                    .num_columns(2)
                    .spacing([24.0, 4.0])
                    .show(ui, |ui| {
                        for (k, v) in [
                            ("Downloaded", format::bytes(a.downloaded)),
                            ("Uploaded", format::bytes(a.uploaded)),
                            ("Ratio", format::ratio(a.uploaded, a.downloaded)),
                            ("Time running", format::duration(running)),
                        ] {
                            ui.label(RichText::new(k).weak());
                            ui.label(v);
                            ui.end_row();
                        }
                    });
                ui.add_space(6.0);
                if ui.button("Show log…").clicked() {
                    open_log = true;
                }
                ui.ctx().request_repaint_after(std::time::Duration::from_secs(1));
            });
        self.stats = stats;
        if open_log {
            self.open_log();
        }

        if let Some(log) = &mut self.log {
            let mut open = true;
            egui::Window::new("Log")
                .open(&mut open)
                .default_size([680.0, 420.0])
                .collapsible(false)
                .show(ctx, |ui| log.ui(ui));
            if !open {
                self.log = None;
            }
        }
    }

    pub fn open_log(&mut self) {
        let path = crate::DATA_DIR
            .get()
            .map(|d| d.join("rusttorrent.log"))
            .unwrap_or_default();
        self.log = Some(LogView::read(path));
    }
}

/// The end of the app's log file.
pub struct LogView {
    path: PathBuf,
    text: String,
}

/// How much of the end of the log is shown.
const LOG_TAIL: u64 = 64 * 1024;

impl LogView {
    fn read(path: PathBuf) -> Self {
        let text = tail(&path).unwrap_or_else(|e| format!("The log could not be read: {e}"));
        Self { path, text }
    }

    fn ui(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if ui.button("Reload").clicked() {
                *self = Self::read(std::mem::take(&mut self.path));
            }
            if ui.button(crate::app::REVEAL).clicked() {
                reveal_file(&self.path);
            }
            ui.add(egui::Label::new(RichText::new(self.path.display().to_string()).small().weak()).truncate());
        });
        ui.separator();
        egui::ScrollArea::both()
            .auto_shrink(false)
            .stick_to_bottom(true)
            .show(ui, |ui| {
                if self.text.is_empty() {
                    ui.label(RichText::new("Nothing has been logged yet.").weak());
                } else {
                    ui.add(egui::Label::new(RichText::new(&self.text).monospace().size(11.0)).extend());
                }
            });
    }
}

/// The last part of a file, starting at a whole line.
fn tail(path: &Path) -> std::io::Result<String> {
    let mut file = match std::fs::File::open(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(String::new()),
        other => other?,
    };
    let len = file.metadata()?.len();
    let start = len.saturating_sub(LOG_TAIL);
    file.seek(SeekFrom::Start(start))?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    let text = String::from_utf8_lossy(&bytes);
    let text = match (start > 0, text.find('\n')) {
        (true, Some(i)) => &text[i + 1..],
        _ => &text,
    };
    Ok(text.to_owned())
}

/// Shows a file in the file manager, selected if the system can do that.
fn reveal_file(path: &Path) {
    let mut command = if cfg!(target_os = "macos") {
        let mut c = std::process::Command::new("open");
        c.arg("-R").arg(path);
        c
    } else if cfg!(windows) {
        let mut c = std::process::Command::new("explorer");
        c.arg(format!("/select,{}", path.display()));
        c
    } else {
        let mut c = std::process::Command::new("xdg-open");
        c.arg(path.parent().unwrap_or(path));
        c
    };
    let _ = command.spawn();
}

const DOWN: Color32 = Color32::from_rgb(64, 156, 255);
const UP: Color32 = Color32::from_rgb(52, 199, 123);

/// Download and upload speed over the last few minutes, newest on the right.
pub fn speed_graph(ui: &mut egui::Ui, speeds: &SpeedHistory, size: Vec2, labels: bool) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    if !ui.is_rect_visible(rect) {
        return response;
    }
    let visuals = ui.visuals();
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 3.0, visuals.extreme_bg_color);
    let series = speeds.series(now(), HISTORY_SECS);
    let peak = series.iter().map(|&(d, u)| d.max(u)).max().unwrap_or(0);
    // Leave a little room above the highest point.
    let top = peak.max(1024) as f32 * 1.15;
    let inner = rect.shrink(1.5);
    let last = series.len().saturating_sub(1).max(1) as f32;
    let at = |i: usize, v: u64| {
        Pos2::new(
            inner.left() + inner.width() * i as f32 / last,
            inner.bottom() - inner.height() * (v as f32 / top),
        )
    };
    let (down, up) = (ink(DOWN), ink(UP));

    // The area under the download line, then both lines.
    let mut mesh = egui::Mesh::default();
    let shade = down.gamma_multiply(0.25);
    for (i, &(d, _)) in series.iter().enumerate() {
        let p = at(i, d);
        mesh.colored_vertex(p, shade);
        mesh.colored_vertex(Pos2::new(p.x, inner.bottom()), shade);
        if i > 0 {
            let k = (i as u32) * 2;
            mesh.add_triangle(k - 2, k - 1, k);
            mesh.add_triangle(k - 1, k, k + 1);
        }
    }
    painter.add(Shape::mesh(mesh));
    let width = if labels { 1.5 } else { 1.0 };
    let line = |pick: fn(&(u64, u64)) -> u64| series.iter().enumerate().map(|(i, s)| at(i, pick(s))).collect();
    painter.add(Shape::line(line(|s| s.1), Stroke::new(width, up)));
    painter.add(Shape::line(line(|s| s.0), Stroke::new(width, down)));

    if labels {
        let font = egui::FontId::proportional(10.0);
        let weak = visuals.weak_text_color();
        let pad = Vec2::new(4.0, 2.0);
        painter.text(
            rect.left_top() + pad,
            egui::Align2::LEFT_TOP,
            if peak > 0 {
                format!("peak {}", format::speed(peak))
            } else {
                "idle".to_owned()
            },
            font.clone(),
            weak,
        );
        painter.text(
            rect.left_bottom() + Vec2::new(pad.x, -pad.y),
            egui::Align2::LEFT_BOTTOM,
            "5 min ago",
            font.clone(),
            weak,
        );
        painter.text(rect.right_bottom() - pad, egui::Align2::RIGHT_BOTTOM, "now", font, weak);
        let mid = inner.top() + inner.height() / 2.0;
        painter.hline(inner.x_range(), mid, Stroke::new(0.5, weak.gamma_multiply(0.4)));
    }
    response
}

/// The small graph in the toolbar. A click opens the statistics.
pub fn sparkline(ui: &mut egui::Ui, extras: &mut Extras, snap: &Snapshot) {
    let response = speed_graph(ui, &snap.speeds, Vec2::new(90.0, 20.0), false);
    if response
        .on_hover_text("Speed over the last 5 minutes. Click for statistics.")
        .clicked()
    {
        extras.stats = !extras.stats;
    }
    if snap.down_bps > 0 || snap.up_bps > 0 {
        ui.ctx().request_repaint_after(std::time::Duration::from_secs(1));
    }
}

/// A thin bar that shows which parts of the torrent are downloaded.
pub fn piece_strip(ui: &mut egui::Ui, map: &PieceMap, fill: Color32) {
    let (rect, response) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 8.0), Sense::hover());
    let painter = ui.painter_at(rect);
    let empty = ui.visuals().extreme_bg_color;
    painter.rect_filled(rect, 2.0, empty);
    let n = map.cells.len();
    if n > 0 {
        let w = rect.width() / n as f32;
        let mut i = 0;
        while i < n {
            let v = map.cells[i];
            let mut j = i + 1;
            while j < n && map.cells[j] == v {
                j += 1;
            }
            if v > 0 {
                let x = rect.left() + i as f32 * w..=rect.left() + j as f32 * w;
                let c = empty.lerp_to_gamma(fill, v as f32 / 255.0);
                painter.rect_filled(Rect::from_x_y_ranges(x, rect.y_range()), 0.0, c);
            }
            i = j;
        }
    }
    response.on_hover_text(format!("Which parts are downloaded. {} pieces.", map.pieces));
}

/// The host name of a tracker address, such as "tracker.example.org".
pub fn host(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let rest = rest.split(['/', '?']).next().unwrap_or(rest);
    let rest = rest.rsplit_once('@').map_or(rest, |(_, h)| h);
    let host = match rest.strip_prefix('[') {
        Some(r) => r.split(']').next().unwrap_or(r),
        None => rest.split(':').next().unwrap_or(rest),
    };
    host.to_lowercase()
}

/// Whether a torrent uses a tracker on this host. An empty host means "no tracker at all".
pub fn has_tracker(t: &TorrentView, chosen: &str) -> bool {
    if chosen.is_empty() {
        t.trackers.is_empty()
    } else {
        t.trackers.iter().any(|u| host(u) == chosen)
    }
}

/// The TRACKERS part of the sidebar. Returns true when the choice changed.
pub fn tracker_section(ui: &mut egui::Ui, torrents: &[TorrentView], chosen: &mut Option<String>) -> bool {
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut none = 0;
    for t in torrents {
        let mut hosts: Vec<String> = t.trackers.iter().map(|u| host(u)).filter(|h| !h.is_empty()).collect();
        hosts.sort();
        hosts.dedup();
        if hosts.is_empty() {
            none += 1;
        }
        for h in hosts {
            *counts.entry(h).or_default() += 1;
        }
    }
    let mut entries: Vec<(String, String, usize)> = counts.into_iter().map(|(h, n)| (h.clone(), h, n)).collect();
    if none > 0 && !entries.is_empty() {
        entries.push((String::new(), "No tracker".to_owned(), none));
    }
    // A tracker that no torrent uses any more cannot be filtered on.
    if chosen.as_ref().is_some_and(|c| !entries.iter().any(|(h, _, _)| h == c)) {
        *chosen = None;
        return true;
    }
    if entries.is_empty() {
        return false;
    }
    let mut changed = false;
    ui.add_space(10.0);
    ui.label(RichText::new("TRACKERS").small().weak());
    for (h, label, count) in entries {
        let on = chosen.as_ref() == Some(&h);
        let button = egui::Button::selectable(on, format!("{label}  {count}")).truncate();
        if ui.add(button).clicked() {
            *chosen = if on { None } else { Some(h) };
            changed = true;
        }
    }
    changed
}

/// The columns of the torrent list that can be hidden.
pub const HIDEABLE: [&str; 8] = ["#", "Progress", "Size", "Down", "Up", "Time left", "Peers", "Ratio"];

pub fn is_hidden(hidden: &[String], name: &str) -> bool {
    HIDEABLE.contains(&name) && hidden.iter().any(|h| h == name)
}

/// The menu shown on a right-click on the list's header. Returns the new hidden columns.
pub fn columns_menu(ui: &mut egui::Ui, hidden: &[String]) -> Option<Vec<String>> {
    ui.label(RichText::new("Show columns").weak());
    let mut out = None;
    for name in HIDEABLE {
        let mut on = !is_hidden(hidden, name);
        if ui.checkbox(&mut on, name).clicked() {
            let mut list: Vec<String> = hidden.iter().filter(|h| *h != name).cloned().collect();
            if !on {
                list.push(name.to_owned());
            }
            out = Some(list);
        }
    }
    out
}

/// The part of the Settings window for this module. Returns true to open the statistics
/// (first) or the log (second).
pub fn settings_section(ui: &mut egui::Ui, c: &mut ComfortSettings) -> (bool, bool) {
    ui.label(RichText::new("When a download finishes").weak());
    if cfg!(target_os = "macos") {
        ui.checkbox(&mut c.notify_finished, "Show a notification");
        if let Some(problem) = sys::notification_problem().filter(|_| c.notify_finished) {
            ui.label(
                RichText::new(problem)
                    .small()
                    .color(ink(Color32::from_rgb(230, 170, 60))),
            );
        }
    }
    ui.horizontal(|ui| {
        ui.label("Run a program");
        ui.add(
            egui::TextEdit::singleline(&mut c.on_finish_program)
                .hint_text("/path/to/program")
                .desired_width(f32::INFINITY),
        );
    });
    ui.label(
        RichText::new("The torrent's name and folder are added at the end. Or put %N and %F where they should go.")
            .small()
            .weak(),
    );
    ui.horizontal(|ui| {
        ui.label("When all downloads finish");
        ui.radio_value(&mut c.when_all_done, WhenAllDone::Nothing, "Do nothing");
        ui.radio_value(&mut c.when_all_done, WhenAllDone::Quit, "Quit RustTorrent");
    });
    ui.add_space(6.0);
    if cfg!(target_os = "macos") {
        ui.checkbox(&mut c.keep_awake, "Keep the Mac awake while downloading or uploading");
    }
    ui.horizontal(|ui| {
        ui.label("Look");
        ui.radio_value(&mut c.theme, Theme::System, "Same as the system");
        ui.radio_value(&mut c.theme, Theme::Dark, "Dark");
        ui.radio_value(&mut c.theme, Theme::Light, "Light");
    });
    let mut open = (false, false);
    ui.horizontal(|ui| {
        open.0 = ui.button("Statistics…").clicked();
        open.1 = ui.button("Show log…").clicked();
    });
    open
}

/// `RUSTTORRENT_SHOT_THEME=light|dark` picks the theme for a test picture.
#[cfg(feature = "shot")]
pub(crate) fn shot_theme() -> Option<Theme> {
    match std::env::var("RUSTTORRENT_SHOT_THEME").ok()?.as_str() {
        "light" => Some(Theme::Light),
        "dark" => Some(Theme::Dark),
        _ => None,
    }
}

#[cfg(not(feature = "shot"))]
pub(crate) fn shot_theme() -> Option<Theme> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hosts() {
        assert_eq!(host("udp://tracker.Example.org:1337/announce"), "tracker.example.org");
        assert_eq!(host("https://user:pw@t.example.com/a?b=c"), "t.example.com");
        assert_eq!(host("http://[2001:db8::1]:80/announce"), "2001:db8::1");
        assert_eq!(host("bare.host"), "bare.host");
    }

    #[test]
    fn columns() {
        let hidden = vec!["Name".to_owned(), "Ratio".to_owned()];
        assert!(!is_hidden(&hidden, "Name"));
        assert!(is_hidden(&hidden, "Ratio"));
        assert!(!is_hidden(&hidden, "Peers"));
    }
}
