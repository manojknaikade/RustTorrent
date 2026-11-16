//! The main window: toolbar, status filters, the torrent table, and details for the selected
//! torrent in the same window.
use std::sync::Arc;

use eframe::egui::{self, Color32, RichText, Sense};
use egui_extras::{Column, TableBuilder};
use rusttorrent_core::{format, Command, Core, Feed, HoldReason, Move, Schedule, Snapshot, State, TorrentView};

fn color(state: State) -> Color32 {
    match state {
        State::Downloading => Color32::from_rgb(64, 156, 255),
        State::Seeding => Color32::from_rgb(52, 199, 123),
        State::Paused => Color32::from_rgb(150, 150, 158),
        State::Queued | State::Scheduled => Color32::from_rgb(170, 140, 235),
        State::FetchingInfo | State::Checking => Color32::from_rgb(230, 170, 60),
        State::Completed => Color32::from_rgb(120, 200, 190),
        State::Blocked | State::Error => Color32::from_rgb(240, 90, 90),
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Filter {
    All,
    Downloading,
    Seeding,
    Queued,
    Paused,
    Completed,
    Error,
}

impl Filter {
    const ALL: [Filter; 7] = [
        Filter::All,
        Filter::Downloading,
        Filter::Seeding,
        Filter::Queued,
        Filter::Paused,
        Filter::Completed,
        Filter::Error,
    ];

    fn label(self) -> &'static str {
        match self {
            Filter::All => "All",
            Filter::Downloading => "Downloading",
            Filter::Seeding => "Seeding",
            Filter::Queued => "Queued",
            Filter::Paused => "Paused",
            Filter::Completed => "Completed",
            Filter::Error => "Error",
        }
    }

    fn dot(self) -> Option<Color32> {
        Some(color(match self {
            Filter::All => return None,
            Filter::Downloading => State::Downloading,
            Filter::Seeding => State::Seeding,
            Filter::Queued => State::Queued,
            Filter::Paused => State::Paused,
            Filter::Completed => State::Completed,
            Filter::Error => State::Error,
        }))
    }

    fn matches(self, state: State) -> bool {
        match self {
            Filter::All => true,
            Filter::Downloading => matches!(state, State::Downloading | State::FetchingInfo | State::Checking),
            Filter::Seeding => state == State::Seeding,
            Filter::Queued => matches!(state, State::Queued | State::Scheduled | State::Blocked),
            Filter::Paused => state == State::Paused,
            Filter::Completed => state == State::Completed,
            Filter::Error => state == State::Error,
        }
    }
}

#[derive(PartialEq, Clone, Copy)]
enum Tab {
    Overview,
    Files,
    Peers,
    Trackers,
}

struct RemoveDialog {
    keys: Vec<String>,
    names: Vec<String>,
    delete_files: bool,
}

struct SettingsDialog {
    down_kib: u32,
    up_kib: u32,
    max_downloads: u32,
    ratio_on: bool,
    /// The ratio itself (1.5), not the percent the core stores.
    ratio: f32,
    schedule: Schedule,
    watch_on: bool,
    watch_dir: Option<std::path::PathBuf>,
    /// The one network interface to use, or `None` for any.
    interface: Option<String>,
    remote_on: bool,
    remote_port: u16,
    remote_lan: bool,
    /// A new password for remote control. Empty keeps the one already set.
    remote_password: String,
    /// The app macOS opens magnet links with, by its bundle identifier. Mac only.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    magnet_handler: Option<String>,
    remote_https: bool,
}

/// Asks for the name of a new category or tag for some torrents.
struct NameDialog {
    keys: Vec<String>,
    tag: bool,
    text: String,
}

struct FeedsDialog {
    feeds: Vec<Feed>,
    /// The category of each feed as text, same order as `feeds`.
    categories: Vec<String>,
}

impl FeedsDialog {
    fn of(snap: &Snapshot) -> Self {
        let feeds: Vec<Feed> = snap.feeds.iter().map(|f| f.feed.clone()).collect();
        let categories = feeds.iter().map(|f| f.category.clone().unwrap_or_default()).collect();
        Self { feeds, categories }
    }
}

/// Every category and every tag in use, each sorted by name.
fn names_in_use(snap: &Snapshot) -> (Vec<String>, Vec<String>) {
    let mut categories: Vec<String> = snap
        .torrents
        .iter()
        .filter_map(|t| t.category.clone())
        .chain(snap.feeds.iter().filter_map(|f| f.feed.category.clone()))
        .collect();
    let mut tags: Vec<String> = snap.torrents.iter().flat_map(|t| t.tags.iter().cloned()).collect();
    for list in [&mut categories, &mut tags] {
        list.sort_by_key(|n| n.to_lowercase());
        list.dedup();
    }
    (categories, tags)
}

impl SettingsDialog {
    fn of(s: &rusttorrent_core::Settings) -> Self {
        Self {
            down_kib: s.down_limit_kib,
            up_kib: s.up_limit_kib,
            max_downloads: s.max_downloads,
            ratio_on: s.ratio_limit_pct > 0,
            ratio: if s.ratio_limit_pct > 0 {
                s.ratio_limit_pct as f32 / 100.0
            } else {
                1.0
            },
            schedule: s.schedule,
            watch_on: s.watch_dir.is_some(),
            watch_dir: s.watch_dir.clone(),
            interface: s.bind_interface.clone(),
            remote_on: s.remote.enabled,
            remote_port: s.remote.port,
            remote_lan: s.remote.lan,
            remote_password: String::new(),
            #[cfg(target_os = "macos")]
            magnet_handler: crate::mac::magnet_handler(),
            #[cfg(not(target_os = "macos"))]
            magnet_handler: None,
            remote_https: s.remote.https,
        }
    }
}

const DAYS: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];

fn clock_text(min: u16) -> String {
    format!("{:02}:{:02}", min / 60, min % 60)
}

/// Hour and minute boxes for a time kept as minutes after midnight.
fn time_edit(ui: &mut egui::Ui, min: &mut u16) {
    let (mut h, mut m) = (*min / 60, *min % 60);
    ui.add(
        egui::DragValue::new(&mut h)
            .range(0..=23)
            .speed(0.1)
            .custom_formatter(|v, _| format!("{v:02}")),
    );
    ui.label(":");
    ui.add(
        egui::DragValue::new(&mut m)
            .range(0..=59)
            .speed(0.2)
            .custom_formatter(|v, _| format!("{v:02}")),
    );
    *min = h * 60 + m;
}

fn ratio_limit_text(pct: u32) -> String {
    if pct == 0 {
        "none".to_owned()
    } else {
        format!("{:.2}", pct as f64 / 100.0)
    }
}

pub struct App {
    core: Core,
    snap: Arc<Snapshot>,
    /// Indexes into `snap.torrents`, filtered and sorted.
    view: Vec<usize>,
    selected: Vec<String>,
    /// The torrent the details panel shows.
    focus: Option<String>,
    filter: Filter,
    /// Show only this category, and only this tag.
    category: Option<String>,
    tag: Option<String>,
    search: String,
    /// Column and direction. Column 0 means newest first.
    sort: (usize, bool),
    tab: Tab,
    dirty: bool,
    /// The torrent the core was last asked to report peers for.
    watching: Option<String>,
    add: Option<String>,
    remove: Option<RemoveDialog>,
    settings: Option<SettingsDialog>,
    name: Option<NameDialog>,
    feeds: Option<FeedsDialog>,
}

fn ratio_of(t: &TorrentView) -> f64 {
    t.uploaded as f64 / t.done.max(1) as f64
}

fn secs_left(t: &TorrentView) -> u64 {
    if t.down_bps == 0 || t.done >= t.total {
        u64::MAX
    } else {
        (t.total - t.done) / t.down_bps
    }
}

fn reveal(path: &str) {
    let program = if cfg!(target_os = "macos") {
        "open"
    } else if cfg!(windows) {
        "explorer"
    } else {
        "xdg-open"
    };
    let _ = std::process::Command::new(program).arg(path).spawn();
}

const REVEAL: &str = if cfg!(target_os = "macos") {
    "Show in Finder"
} else {
    "Open folder"
};

fn bar(ui: &mut egui::Ui, t: &TorrentView) {
    let text = match t.state {
        State::FetchingInfo => "Fetching info".to_owned(),
        State::Queued => format!("Queued  {:.0}%", t.progress() * 100.0),
        State::Scheduled => format!("Scheduled  {:.0}%", t.progress() * 100.0),
        State::Blocked => format!("Blocked  {:.0}%", t.progress() * 100.0),
        _ => format!("{:.1}%", t.progress() * 100.0),
    };
    ui.add(
        egui::ProgressBar::new(t.progress())
            .fill(color(t.state).gamma_multiply(0.75))
            .text(RichText::new(text).size(11.0))
            .desired_height(14.0),
    );
}

impl App {
    pub fn new(core: Core) -> Self {
        let snap = core.snapshot();
        Self {
            core,
            snap,
            view: Vec::new(),
            selected: Vec::new(),
            focus: None,
            filter: Filter::All,
            category: None,
            tag: None,
            search: String::new(),
            sort: (0, false),
            tab: Tab::Overview,
            dirty: true,
            watching: None,
            add: None,
            remove: None,
            settings: None,
            name: None,
            feeds: None,
        }
    }

    fn rebuild(&mut self) {
        let all = &self.snap.torrents;
        let q = self.search.to_lowercase();
        // A category or tag that no torrent has any more cannot be filtered on.
        if self
            .category
            .as_ref()
            .is_some_and(|c| !all.iter().any(|t| t.category.as_ref() == Some(c)))
        {
            self.category = None;
        }
        if self
            .tag
            .as_ref()
            .is_some_and(|g| !all.iter().any(|t| t.tags.contains(g)))
        {
            self.tag = None;
        }
        self.view = (0..all.len())
            .filter(|&i| {
                let t = &all[i];
                self.filter.matches(t.state)
                    && self.category.as_ref().is_none_or(|c| t.category.as_ref() == Some(c))
                    && self.tag.as_ref().is_none_or(|g| t.tags.contains(g))
                    && (q.is_empty() || t.name.to_lowercase().contains(&q))
            })
            .collect();
        let (col, asc) = self.sort;
        self.view.sort_by(|&a, &b| {
            let (a, b) = (&all[a], &all[b]);
            let order = match col {
                1 => a.queue_pos.unwrap_or(u32::MAX).cmp(&b.queue_pos.unwrap_or(u32::MAX)),
                2 => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
                3 => a.progress().total_cmp(&b.progress()),
                4 => a.total.cmp(&b.total),
                5 => a.down_bps.cmp(&b.down_bps),
                6 => a.up_bps.cmp(&b.up_bps),
                7 => secs_left(a).cmp(&secs_left(b)),
                8 => a.peers_live.cmp(&b.peers_live),
                9 => ratio_of(a).total_cmp(&ratio_of(b)),
                _ => a.added_at.cmp(&b.added_at),
            };
            let order = if asc { order } else { order.reverse() };
            order.then_with(|| a.key.cmp(&b.key))
        });
        self.selected.retain(|k| all.iter().any(|t| &t.key == k));
        if self.focus.as_ref().is_some_and(|k| !all.iter().any(|t| &t.key == k)) {
            self.focus = self.selected.first().cloned();
        }
        self.dirty = false;
    }

    fn ask_remove(&mut self, keys: Vec<String>) {
        if keys.is_empty() {
            return;
        }
        let names = self
            .snap
            .torrents
            .iter()
            .filter(|t| keys.contains(&t.key))
            .map(|t| t.name.clone())
            .collect();
        self.remove = Some(RemoveDialog {
            keys,
            names,
            delete_files: false,
        });
    }

    fn top_bar(&mut self, ui: &mut egui::Ui) {
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            if ui.button(RichText::new("+ Add torrent").strong()).clicked() {
                self.add = Some(String::new());
            }
            let any = !self.selected.is_empty();
            if ui.add_enabled(any, egui::Button::new("Pause")).clicked() {
                self.core.send(Command::Pause(self.selected.clone()));
            }
            if ui.add_enabled(any, egui::Button::new("Resume")).clicked() {
                self.core.send(Command::Resume(self.selected.clone()));
            }
            if ui.add_enabled(any, egui::Button::new("Remove")).clicked() {
                self.ask_remove(self.selected.clone());
            }
            ui.separator();
            let search = egui::TextEdit::singleline(&mut self.search)
                .hint_text("Search names")
                .desired_width(260.0);
            if ui.add(search).changed() {
                self.dirty = true;
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Settings").clicked() {
                    let s = &self.snap.settings;
                    self.settings = Some(SettingsDialog::of(s));
                }
                let failing = self.snap.feeds.iter().any(|f| f.feed.enabled && f.error.is_some());
                let feeds = if failing {
                    RichText::new("Feeds !").color(color(State::Error))
                } else {
                    RichText::new("Feeds")
                };
                if ui.button(feeds).clicked() {
                    self.feeds = Some(FeedsDialog::of(&self.snap));
                }
                ui.separator();
                ui.label(format!("↑ {}", format::speed(self.snap.up_bps)));
                ui.label(RichText::new(format!("↓ {}", format::speed(self.snap.down_bps))).strong());
            });
        });
        ui.add_space(6.0);
    }

    fn status_bar(&mut self, ui: &mut egui::Ui) {
        ui.add_space(3.0);
        ui.horizontal(|ui| {
            if !self.snap.ready {
                ui.label(RichText::new("Starting…").weak());
            } else if let Some(n) = self.snap.notices.last() {
                let text = RichText::new(&n.text).small();
                let text = if n.error {
                    text.color(color(State::Error))
                } else {
                    text.weak()
                };
                ui.add(egui::Label::new(text).truncate());
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let s = &self.snap.settings;
                let limit = |kib: u32| {
                    if kib == 0 {
                        "no limit".to_owned()
                    } else {
                        format!("{kib} KiB/s")
                    }
                };
                let text = format!("Limits: ↓ {}  ↑ {}", limit(s.down_limit_kib), limit(s.up_limit_kib));
                ui.label(RichText::new(text).small().weak());
                if self.snap.schedule_off {
                    let text = format!(
                        "Transfers are off by schedule until {}",
                        clock_text(s.schedule.start_min)
                    );
                    ui.label(RichText::new(text).small().color(color(State::Scheduled)));
                }
                if let (Some(name), None) = (&s.bind_interface, &self.snap.blocked) {
                    ui.label(RichText::new(format!("via {name}")).small().weak());
                }
            });
        });
        ui.add_space(3.0);
    }

    fn sidebar(&mut self, ui: &mut egui::Ui) {
        ui.add_space(8.0);
        ui.label(RichText::new("STATUS").small().weak());
        for f in Filter::ALL {
            let count = self.snap.torrents.iter().filter(|t| f.matches(t.state)).count();
            let text = format!("{}  {count}", f.label());
            let on = self.filter == f;
            let text = match f.dot() {
                Some(c) => RichText::new(format!("● {text}")).color(if on { Color32::WHITE } else { c }),
                None => RichText::new(text),
            };
            if ui.selectable_label(on, text).clicked() {
                self.filter = f;
                self.dirty = true;
            }
        }
        let snap = self.snap.clone();
        let (categories, tags) = names_in_use(&snap);
        egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            for (title, names, tag) in [("CATEGORIES", categories, false), ("TAGS", tags, true)] {
                let names: Vec<(String, usize)> = names
                    .into_iter()
                    .map(|n| {
                        let has = |t: &&TorrentView| {
                            if tag {
                                t.tags.contains(&n)
                            } else {
                                t.category.as_ref() == Some(&n)
                            }
                        };
                        let count = snap.torrents.iter().filter(has).count();
                        (n, count)
                    })
                    .filter(|(_, count)| *count > 0)
                    .collect();
                if names.is_empty() {
                    continue;
                }
                ui.add_space(10.0);
                ui.label(RichText::new(title).small().weak());
                let chosen = if tag { &mut self.tag } else { &mut self.category };
                for (name, count) in names {
                    let on = chosen.as_ref() == Some(&name);
                    let label = egui::Button::selectable(on, format!("{name}  {count}")).truncate();
                    // A second click shows everything again.
                    if ui.add(label).clicked() {
                        *chosen = if on { None } else { Some(name) };
                        self.dirty = true;
                    }
                }
            }
        });
    }

    fn details(&mut self, ui: &mut egui::Ui) {
        let snap = self.snap.clone();
        let Some(t) = self
            .focus
            .as_ref()
            .and_then(|k| snap.torrents.iter().find(|t| &t.key == k))
        else {
            ui.centered_and_justified(|ui| {
                ui.label(RichText::new("Select a torrent to see its details").weak());
            });
            return;
        };
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.add(egui::Label::new(RichText::new(&t.name).strong().size(15.0)).truncate());
        });
        ui.horizontal(|ui| {
            for (tab, name) in [
                (Tab::Overview, "Overview"),
                (Tab::Files, "Files"),
                (Tab::Peers, "Peers"),
                (Tab::Trackers, "Trackers"),
            ] {
                ui.selectable_value(&mut self.tab, tab, name);
            }
            ui.separator();
            ui.label(RichText::new(t.state.label()).color(color(t.state)));
        });
        ui.separator();
        match self.tab {
            Tab::Overview => self.overview(ui, t),
            Tab::Files => self.files(ui, t),
            Tab::Peers => peers(ui, t),
            Tab::Trackers => trackers(ui, t),
        }
    }

    fn overview(&mut self, ui: &mut egui::Ui, t: &TorrentView) {
        if let Some(e) = &t.error {
            ui.label(RichText::new(e).color(color(State::Error)));
            ui.add_space(4.0);
        }
        ui.add(
            egui::ProgressBar::new(t.progress())
                .fill(color(t.state).gamma_multiply(0.8))
                .text(RichText::new(format!("{:.1}%", t.progress() * 100.0)).size(11.0))
                .desired_height(18.0),
        );
        ui.add_space(6.0);
        egui::Grid::new("overview")
            .num_columns(6)
            .spacing([28.0, 6.0])
            .show(ui, |ui| {
                let row = |ui: &mut egui::Ui, items: [(&str, String); 3]| {
                    for (k, v) in items {
                        ui.label(RichText::new(k).weak());
                        ui.label(v);
                    }
                    ui.end_row();
                };
                row(
                    ui,
                    [
                        (
                            "Downloaded",
                            format!("{} of {}", format::bytes(t.done), format::bytes(t.total)),
                        ),
                        ("Download", format::speed(t.down_bps)),
                        ("Upload", format::speed(t.up_bps)),
                    ],
                );
                row(
                    ui,
                    [
                        ("Time left", format::eta(t.total.saturating_sub(t.done), t.down_bps)),
                        ("Peers", format!("{} connected, {} known", t.peers_live, t.peers_seen)),
                        ("Ratio", format::ratio(t.uploaded, t.done)),
                    ],
                );
                let chosen = match &t.selected {
                    Some(s) => format!("{} of {} chosen", s.len(), t.files.len()),
                    None => format!("{}", t.files.len()),
                };
                let age = rusttorrent_core::format::duration(now().saturating_sub(t.added_at));
                row(
                    ui,
                    [
                        ("Uploaded", format::bytes(t.uploaded)),
                        ("Files", chosen),
                        ("Added", format!("{age} ago")),
                    ],
                );
                let limit = t.ratio_limit_pct.unwrap_or(self.snap.settings.ratio_limit_pct);
                let own = if t.ratio_limit_pct.is_some() {
                    " (this torrent)"
                } else {
                    ""
                };
                let waiting = match t.held {
                    None => "–",
                    Some(HoldReason::Queue) => "A free download slot",
                    Some(HoldReason::Schedule) => "The schedule",
                    Some(HoldReason::Ratio) => "Nothing: ratio limit reached",
                    Some(HoldReason::Network) => "The network",
                };
                row(
                    ui,
                    [
                        ("Queue place", t.queue_pos.map_or("–".to_owned(), |p| p.to_string())),
                        ("Ratio limit", format!("{}{own}", ratio_limit_text(limit))),
                        ("Waiting for", waiting.to_owned()),
                    ],
                );
            });
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new("Category").weak());
            ui.label(t.category.as_deref().unwrap_or("–"));
            ui.add_space(20.0);
            ui.label(RichText::new("Tags").weak());
            if t.tags.is_empty() {
                ui.label("–");
            } else {
                ui.add(egui::Label::new(t.tags.join(", ")).truncate());
            }
        });
        ui.horizontal(|ui| {
            ui.label(RichText::new("Saved to").weak());
            ui.add(egui::Label::new(&t.save_path).truncate());
        });
        ui.horizontal(|ui| {
            if ui.button(REVEAL).clicked() {
                reveal(&t.save_path);
            }
            if !t.is_pending() {
                if ui.button("Copy info-hash").clicked() {
                    ui.ctx().copy_text(t.key.clone());
                }
                ui.label(RichText::new(&t.key).small().weak());
            }
        });
    }

    fn files(&mut self, ui: &mut egui::Ui, t: &TorrentView) {
        let n = t.files.len();
        if n == 0 {
            ui.label(RichText::new("The file list has not arrived yet.").weak());
            return;
        }
        let is_on = |i: usize| t.selected.as_ref().is_none_or(|s| s.contains(&i));
        let mut change: Option<Vec<usize>> = None;
        ui.horizontal(|ui| {
            let chosen = t.selected.as_ref().map_or(n, |s| s.len());
            ui.label(RichText::new(format!("{chosen} of {n} files chosen for download")).weak());
            if chosen < n && ui.small_button("Choose all").clicked() {
                change = Some((0..n).collect());
            }
        });
        if n > 1 || t.done < t.total {
            ui.label(
                RichText::new("Downloads in order: files one after another by name, each from its start to its end.")
                    .small()
                    .weak(),
            );
        }
        egui::ScrollArea::vertical()
            .auto_shrink(false)
            .show_rows(ui, 20.0, n, |ui, range| {
                for i in range {
                    let f = &t.files[i];
                    ui.horizontal(|ui| {
                        let mut on = is_on(i);
                        if ui.checkbox(&mut on, "").changed() {
                            change = Some((0..n).filter(|&k| if k == i { on } else { is_on(k) }).collect());
                        }
                        let done = t.file_done.get(i).copied().unwrap_or(0);
                        let p = if f.len == 0 {
                            1.0
                        } else {
                            (done as f64 / f.len as f64) as f32
                        };
                        ui.add(egui::ProgressBar::new(p).desired_width(110.0).desired_height(10.0));
                        ui.add_sized(
                            [70.0, 18.0],
                            egui::Label::new(RichText::new(format::bytes(f.len)).weak()),
                        );
                        ui.add(egui::Label::new(&f.name).truncate());
                    });
                }
            });
        if let Some(files) = change {
            if files.is_empty() {
                // The engine needs at least one file. To stop everything, pause the torrent.
                return;
            }
            self.core.send(Command::SetFiles {
                key: t.key.clone(),
                files,
            });
        }
    }

    fn table(&mut self, ui: &mut egui::Ui) {
        let snap = self.snap.clone();
        let all = &snap.torrents;
        if all.is_empty() {
            ui.centered_and_justified(|ui| {
                let text = if snap.ready {
                    "No torrents yet.\nDrop a .torrent file here, or click “+ Add torrent” to paste a magnet link."
                } else {
                    "Starting…"
                };
                ui.label(RichText::new(text).weak());
            });
            return;
        }
        let heads = [
            "",
            "#",
            "Name",
            "Progress",
            "Size",
            "Down",
            "Up",
            "Time left",
            "Peers",
            "Ratio",
        ];
        let view = &self.view;
        let selected = &self.selected;
        let mut sort = self.sort;
        let mut clicked: Option<usize> = None;
        let mut action: Option<(Command, bool)> = None;
        // Set when the menu asks for a new category (false) or tag (true).
        let mut ask_name: Option<(Vec<String>, bool)> = None;
        let (categories, tags) = names_in_use(&snap);
        TableBuilder::new(ui)
            .striped(true)
            .sense(Sense::click())
            .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
            .column(Column::exact(14.0))
            .column(Column::exact(30.0))
            .column(Column::remainder().at_least(200.0).clip(true))
            .column(Column::exact(150.0))
            .column(Column::exact(80.0))
            .column(Column::exact(90.0))
            .column(Column::exact(90.0))
            .column(Column::exact(80.0))
            .column(Column::exact(50.0))
            .column(Column::exact(50.0))
            .header(24.0, |mut h| {
                for (i, name) in heads.iter().enumerate() {
                    h.col(|ui| {
                        let mark = match sort {
                            (c, asc) if c == i && i > 0 => {
                                if asc {
                                    " ▲"
                                } else {
                                    " ▼"
                                }
                            }
                            _ => "",
                        };
                        let label =
                            egui::Label::new(RichText::new(format!("{name}{mark}")).strong()).sense(Sense::click());
                        if ui.add(label).clicked() && i > 0 {
                            sort = if sort.0 == i { (i, !sort.1) } else { (i, i <= 2) };
                        }
                    });
                }
            })
            .body(|body| {
                body.rows(26.0, view.len(), |mut row| {
                    let idx = view[row.index()];
                    let t = &all[idx];
                    row.set_selected(selected.contains(&t.key));
                    row.col(|ui| {
                        ui.label(RichText::new("●").color(color(t.state)));
                    });
                    row.col(|ui| {
                        if let Some(p) = t.queue_pos {
                            ui.label(RichText::new(p.to_string()).weak());
                        }
                    });
                    row.col(|ui| {
                        ui.add(egui::Label::new(&t.name).truncate().selectable(false));
                    });
                    row.col(|ui| bar(ui, t));
                    row.col(|ui| {
                        ui.label(if t.total == 0 {
                            "–".to_owned()
                        } else {
                            format::bytes(t.total)
                        });
                    });
                    row.col(|ui| {
                        ui.label(format::speed(t.down_bps));
                    });
                    row.col(|ui| {
                        ui.label(format::speed(t.up_bps));
                    });
                    row.col(|ui| {
                        ui.label(format::eta(t.total.saturating_sub(t.done), t.down_bps));
                    });
                    row.col(|ui| {
                        ui.label(t.peers_live.to_string());
                    });
                    row.col(|ui| {
                        ui.label(format::ratio(t.uploaded, t.done));
                    });
                    let response = row.response();
                    if response.clicked() {
                        clicked = Some(idx);
                    }
                    response.context_menu(|ui| {
                        // Acts on the whole selection when the row is part of it.
                        let key = if selected.contains(&t.key) {
                            selected.clone()
                        } else {
                            vec![t.key.clone()]
                        };
                        if ui.button("Pause").clicked() {
                            action = Some((Command::Pause(key.clone()), false));
                            ui.close();
                        }
                        if ui.button("Resume").clicked() {
                            action = Some((Command::Resume(key.clone()), false));
                            ui.close();
                        }
                        if ui.button(REVEAL).clicked() {
                            reveal(&t.save_path);
                            ui.close();
                        }
                        ui.separator();
                        // The engine has no other order, so this cannot be switched off.
                        let mut in_order = true;
                        ui.add_enabled(false, egui::Checkbox::new(&mut in_order, "Download in order"))
                            .on_disabled_hover_text(
                                "Always on. Files download one after another by name, each from its \
                                 start to its end, so a video can be played while it downloads.",
                            );
                        ui.menu_button("Queue", |ui| {
                            for (name, how) in [
                                ("Move to top", Move::Top),
                                ("Move up", Move::Up),
                                ("Move down", Move::Down),
                                ("Move to bottom", Move::Bottom),
                            ] {
                                if ui.button(name).clicked() {
                                    action = Some((Command::MoveInQueue { keys: key.clone(), how }, false));
                                    ui.close();
                                }
                            }
                        });
                        ui.menu_button("Ratio limit", |ui| {
                            for (name, limit) in [
                                ("Use the setting", None),
                                ("No limit", Some(0)),
                                ("Stop at 1.0", Some(100)),
                                ("Stop at 2.0", Some(200)),
                                ("Stop at 3.0", Some(300)),
                            ] {
                                if ui.radio(t.ratio_limit_pct == limit, name).clicked() {
                                    action = Some((
                                        Command::SetRatioLimit {
                                            keys: key.clone(),
                                            limit,
                                        },
                                        false,
                                    ));
                                    ui.close();
                                }
                            }
                        });
                        ui.menu_button("Category", |ui| {
                            if ui.radio(t.category.is_none(), "None").clicked() {
                                action = Some((
                                    Command::SetCategory {
                                        keys: key.clone(),
                                        category: None,
                                    },
                                    false,
                                ));
                                ui.close();
                            }
                            for name in &categories {
                                if ui.radio(t.category.as_ref() == Some(name), name).clicked() {
                                    action = Some((
                                        Command::SetCategory {
                                            keys: key.clone(),
                                            category: Some(name.clone()),
                                        },
                                        false,
                                    ));
                                    ui.close();
                                }
                            }
                            ui.separator();
                            if ui.button("New…").clicked() {
                                ask_name = Some((key.clone(), false));
                                ui.close();
                            }
                        });
                        ui.menu_button("Tags", |ui| {
                            for name in &tags {
                                let mut on = t.tags.contains(name);
                                if ui.checkbox(&mut on, name).clicked() {
                                    action = Some((
                                        Command::SetTag {
                                            keys: key.clone(),
                                            tag: name.clone(),
                                            on,
                                        },
                                        false,
                                    ));
                                    ui.close();
                                }
                            }
                            if !tags.is_empty() {
                                ui.separator();
                            }
                            if ui.button("New…").clicked() {
                                ask_name = Some((key.clone(), true));
                                ui.close();
                            }
                        });
                        ui.separator();
                        if ui.button("Remove…").clicked() {
                            action = Some((
                                Command::Remove {
                                    keys: key,
                                    delete_files: false,
                                },
                                true,
                            ));
                            ui.close();
                        }
                    });
                });
            });

        if let Some(idx) = clicked {
            let key = all[idx].key.clone();
            let (command, shift) = ui.input(|i| (i.modifiers.command, i.modifiers.shift));
            let anchor = self
                .focus
                .as_ref()
                .and_then(|f| self.view.iter().position(|&i| &all[i].key == f));
            let here = self.view.iter().position(|&i| i == idx);
            if command {
                match self.selected.iter().position(|k| *k == key) {
                    Some(i) => {
                        self.selected.remove(i);
                    }
                    None => self.selected.push(key.clone()),
                }
            } else if let (true, Some(a), Some(b)) = (shift, anchor, here) {
                let (lo, hi) = (a.min(b), a.max(b));
                self.selected = self.view[lo..=hi].iter().map(|&i| all[i].key.clone()).collect();
            } else {
                self.selected = vec![key.clone()];
            }
            if !shift {
                self.focus = Some(key);
            }
        }
        match action {
            Some((Command::Remove { keys, .. }, true)) => self.ask_remove(keys),
            Some((command, _)) => self.core.send(command),
            None => {}
        }
        if let Some((keys, tag)) = ask_name {
            self.name = Some(NameDialog {
                keys,
                tag,
                text: String::new(),
            });
        }
        if sort != self.sort {
            self.sort = sort;
            self.dirty = true;
        }
    }

    fn dialogs(&mut self, ctx: &egui::Context) {
        if let Some(text) = &mut self.add {
            let (mut close, mut submit) = (false, false);
            let core = &self.core;
            let response = egui::Modal::new(egui::Id::unique("add")).show(ctx, |ui| {
                ui.set_width(480.0);
                ui.heading("Add torrent");
                ui.add_space(4.0);
                ui.label("Paste magnet links or web addresses of .torrent files, one per line.");
                let edit = egui::TextEdit::multiline(text)
                    .desired_rows(4)
                    .desired_width(f32::INFINITY)
                    .hint_text("magnet:?xt=urn:btih:…");
                let r = ui.add(edit);
                if text.is_empty() && !r.has_focus() {
                    r.request_focus();
                }
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if ui.button("Choose .torrent files…").clicked() {
                        let picked = rfd::FileDialog::new()
                            .add_filter("Torrent files", &["torrent"])
                            .pick_files();
                        if let Some(files) = picked {
                            for f in files {
                                core.send(Command::Add(f.to_string_lossy().into_owned()));
                            }
                            submit = true;
                        }
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let ready = !text.trim().is_empty();
                        if ui
                            .add_enabled(ready, egui::Button::new(RichText::new("Add").strong()))
                            .clicked()
                        {
                            submit = true;
                        }
                        if ui.button("Cancel").clicked() {
                            close = true;
                        }
                    });
                });
            });
            if submit {
                for line in text.lines().filter(|l| !l.trim().is_empty()) {
                    core.send(Command::Add(line.to_owned()));
                }
                close = true;
            }
            if close || response.should_close() {
                self.add = None;
            }
        }

        if let Some(dialog) = &mut self.remove {
            let (mut close, mut confirm) = (false, false);
            let response = egui::Modal::new(egui::Id::unique("remove")).show(ctx, |ui| {
                ui.set_width(420.0);
                let n = dialog.keys.len();
                ui.heading(if n == 1 {
                    "Remove torrent?".to_owned()
                } else {
                    format!("Remove {n} torrents?")
                });
                ui.add_space(4.0);
                for name in dialog.names.iter().take(5) {
                    ui.add(egui::Label::new(name).truncate());
                }
                if dialog.names.len() > 5 {
                    ui.label(RichText::new(format!("and {} more", dialog.names.len() - 5)).weak());
                }
                ui.add_space(6.0);
                ui.checkbox(&mut dialog.delete_files, "Also delete the downloaded files");
                ui.add_space(6.0);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let label = if dialog.delete_files {
                        "Remove and delete files"
                    } else {
                        "Remove"
                    };
                    if ui.button(RichText::new(label).strong()).clicked() {
                        confirm = true;
                    }
                    if ui.button("Cancel").clicked() {
                        close = true;
                    }
                });
            });
            if confirm {
                self.core.send(Command::Remove {
                    keys: dialog.keys.clone(),
                    delete_files: dialog.delete_files,
                });
                close = true;
            }
            if close || response.should_close() {
                self.remove = None;
            }
        }

        if let Some(dialog) = &mut self.settings {
            let (mut close, mut save) = (false, false);
            let core = &self.core;
            let snap = &self.snap;
            let response = egui::Modal::new(egui::Id::unique("settings")).show(ctx, |ui| {
                ui.set_width(460.0);
                ui.heading("Settings");
                ui.add_space(6.0);
                ui.label(RichText::new("New torrents are saved to").weak());
                ui.horizontal(|ui| {
                    if ui.button("Change…").clicked() {
                        let start = &snap.settings.download_dir;
                        if let Some(dir) = rfd::FileDialog::new().set_directory(start).pick_folder() {
                            core.send(Command::SetDownloadDir(dir));
                        }
                    }
                    ui.add(egui::Label::new(snap.settings.download_dir.display().to_string()).truncate());
                });
                ui.add_space(8.0);
                ui.label(RichText::new("Speed limits in KiB/s. 0 means no limit.").weak());
                egui::Grid::new("limits")
                    .num_columns(2)
                    .spacing([12.0, 6.0])
                    .show(ui, |ui| {
                        ui.label("Download");
                        ui.add(
                            egui::DragValue::new(&mut dialog.down_kib)
                                .range(0..=4_000_000)
                                .speed(10),
                        );
                        ui.end_row();
                        ui.label("Upload");
                        ui.add(egui::DragValue::new(&mut dialog.up_kib).range(0..=4_000_000).speed(10));
                        ui.end_row();
                    });
                ui.add_space(8.0);
                ui.label(RichText::new("Queue and seeding").weak());
                egui::Grid::new("rules")
                    .num_columns(2)
                    .spacing([12.0, 6.0])
                    .show(ui, |ui| {
                        ui.label("Downloads at once");
                        ui.horizontal(|ui| {
                            ui.add(
                                egui::DragValue::new(&mut dialog.max_downloads)
                                    .range(0..=100)
                                    .speed(0.1),
                            );
                            ui.label(RichText::new("0 means no limit").weak());
                        });
                        ui.end_row();
                        ui.checkbox(&mut dialog.ratio_on, "Stop seeding at ratio");
                        ui.add_enabled(
                            dialog.ratio_on,
                            egui::DragValue::new(&mut dialog.ratio)
                                .range(0.1..=100.0)
                                .speed(0.05)
                                .fixed_decimals(2),
                        );
                        ui.end_row();
                    });
                ui.add_space(8.0);
                let sch = &mut dialog.schedule;
                ui.checkbox(&mut sch.enabled, "Transfer only at these times");
                ui.add_enabled_ui(sch.enabled, |ui| {
                    ui.horizontal(|ui| {
                        ui.label("From");
                        time_edit(ui, &mut sch.start_min);
                        ui.label("to");
                        time_edit(ui, &mut sch.end_min);
                        if sch.start_min > sch.end_min {
                            ui.label(RichText::new("the next day").weak());
                        } else if sch.start_min == sch.end_min {
                            ui.label(RichText::new("the whole day").weak());
                        }
                    });
                    ui.horizontal(|ui| {
                        ui.label("Starting on");
                        for (i, day) in DAYS.iter().enumerate() {
                            let bit = 1u8 << i;
                            if ui.selectable_label(sch.days & bit != 0, *day).clicked() {
                                sch.days ^= bit;
                            }
                        }
                    });
                });
                ui.add_space(8.0);
                ui.checkbox(&mut dialog.watch_on, "Add .torrent files put in a folder");
                ui.add_enabled_ui(dialog.watch_on, |ui| {
                    ui.horizontal(|ui| {
                        if ui.button("Choose…").clicked() {
                            if let Some(dir) = rfd::FileDialog::new().pick_folder() {
                                dialog.watch_dir = Some(dir);
                            }
                        }
                        let text = match &dialog.watch_dir {
                            Some(dir) => RichText::new(dir.display().to_string()),
                            None => RichText::new("No folder chosen").weak(),
                        };
                        ui.add(egui::Label::new(text).truncate());
                    });
                    ui.label(
                        RichText::new("A file that has been added is renamed to end in “.added”.")
                            .small()
                            .weak(),
                    );
                });
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.label("Network interface");
                    let label = |name: &str| match snap.interfaces.iter().find(|i| i.name == name) {
                        Some(i) => format!("{name}  ({})", i.addrs.join(", ")),
                        None => format!("{name}  (not connected)"),
                    };
                    let shown = match &dialog.interface {
                        Some(name) => label(name),
                        None => "Any".to_owned(),
                    };
                    egui::ComboBox::from_id_salt("interface")
                        .selected_text(shown)
                        .width(260.0)
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut dialog.interface, None, "Any");
                            let mut names: Vec<String> = snap.interfaces.iter().map(|i| i.name.clone()).collect();
                            if let Some(chosen) = &snap.settings.bind_interface {
                                if !names.contains(chosen) {
                                    names.push(chosen.clone());
                                }
                            }
                            for name in names {
                                let text = label(&name);
                                ui.selectable_value(&mut dialog.interface, Some(name), text);
                            }
                        });
                });
                ui.label(
                    RichText::new(
                        "For a VPN: choose its interface and nothing is sent or received any other way. \
                         While that interface is not connected, all traffic is blocked.",
                    )
                    .small()
                    .weak(),
                );
                ui.add_space(8.0);
                ui.checkbox(&mut dialog.remote_on, "Remote control from a browser");
                let has_password = snap.settings.remote.has_password();
                ui.add_enabled_ui(dialog.remote_on, |ui| {
                    ui.horizontal(|ui| {
                        ui.label("Port");
                        ui.add(egui::DragValue::new(&mut dialog.remote_port).range(1024..=65535));
                        ui.checkbox(&mut dialog.remote_lan, "Allow other devices on my network");
                    });
                    ui.horizontal(|ui| {
                        ui.label(if has_password { "New password" } else { "Password" });
                        ui.add(
                            egui::TextEdit::singleline(&mut dialog.remote_password)
                                .password(true)
                                .desired_width(200.0)
                                .hint_text(if has_password {
                                    "unchanged"
                                } else {
                                    "at least 8 characters"
                                }),
                        );
                    });
                    ui.checkbox(&mut dialog.remote_https, "Use HTTPS");
                });
                let typed = dialog.remote_password.chars().count();
                let problem = if typed > 0 && typed < rusttorrent_core::remote::MIN_PASSWORD {
                    Some("The password needs at least 8 characters.")
                } else if dialog.remote_on && !has_password && typed == 0 {
                    Some("Remote control needs a password.")
                } else {
                    None
                };
                let red = ui.visuals().error_fg_color;
                if let Some(problem) = problem {
                    ui.label(RichText::new(problem).small().color(red));
                } else if let Some(error) = &snap.remote_error {
                    ui.label(RichText::new(error).small().color(red));
                } else if snap.remote_on {
                    let port = snap.settings.remote.port;
                    let scheme = if snap.settings.remote.https { "https" } else { "http" };
                    let mut text = format!("On now. On this computer: {scheme}://localhost:{port}");
                    if snap.settings.remote.lan {
                        let other = snap
                            .interfaces
                            .iter()
                            .flat_map(|i| &i.addrs)
                            .find(|a| a.parse::<std::net::Ipv4Addr>().is_ok_and(|ip| !ip.is_loopback()));
                        if let Some(addr) = other {
                            text.push_str(&format!("   From a phone: {scheme}://{addr}:{port}"));
                        }
                    }
                    ui.label(RichText::new(text).small());
                    if let Some(fingerprint) = &snap.remote_fingerprint {
                        ui.label(RichText::new(format!("Certificate fingerprint (SHA-256): {fingerprint}")).small());
                    }
                }
                ui.label(
                    RichText::new(if dialog.remote_https {
                        "The page can add torrents (also from a .torrent file), pause, resume and remove \
                         them, choose their files and category, and set speed limits. With HTTPS it is \
                         encrypted, but the certificate is made by this app, so the browser warns the \
                         first time. Go on only if the fingerprint it shows matches the one here."
                    } else {
                        "The page can add torrents (also from a .torrent file), pause, resume and remove \
                         them, choose their files and category, and set speed limits. Without HTTPS it is \
                         not encrypted: allow other devices only on a network you trust."
                    })
                    .small()
                    .weak(),
                );
                #[cfg(target_os = "macos")]
                {
                    ui.add_space(8.0);
                    ui.separator();
                    let ours = dialog
                        .magnet_handler
                        .as_deref()
                        .is_some_and(|id| id.eq_ignore_ascii_case(crate::mac::BUNDLE_ID));
                    if ours {
                        ui.label("Magnet links clicked in a browser open in RustTorrent.");
                    } else {
                        ui.horizontal(|ui| {
                            ui.label(match &dialog.magnet_handler {
                                Some(id) => format!("Magnet links now open in another app ({id})."),
                                None => "No app is set to open magnet links.".to_owned(),
                            });
                            if ui.button("Open them in RustTorrent").clicked() {
                                crate::mac::make_magnet_handler();
                                dialog.magnet_handler = crate::mac::magnet_handler();
                            }
                        });
                    }
                }
                ui.add_space(8.0);
                ui.label(
                    RichText::new(format!("RustTorrent {}", rusttorrent_core::VERSION))
                        .small()
                        .weak(),
                );
                ui.add_space(4.0);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let button = egui::Button::new(RichText::new("Save").strong());
                    if ui.add_enabled(problem.is_none(), button).clicked() {
                        save = true;
                    }
                    if ui.button("Cancel").clicked() {
                        close = true;
                    }
                });
            });
            if save {
                core.send(Command::SetLimits {
                    down_kib: dialog.down_kib,
                    up_kib: dialog.up_kib,
                });
                core.send(Command::SetRules {
                    max_downloads: dialog.max_downloads,
                    ratio_limit_pct: if dialog.ratio_on {
                        (dialog.ratio * 100.0).round() as u32
                    } else {
                        0
                    },
                    schedule: dialog.schedule,
                });
                let watch = dialog.watch_dir.clone().filter(|_| dialog.watch_on);
                if watch != snap.settings.watch_dir {
                    core.send(Command::SetWatchDir(watch));
                }
                if dialog.interface != snap.settings.bind_interface {
                    core.send(Command::SetInterface(dialog.interface.clone()));
                }
                let remote = &snap.settings.remote;
                let password = Some(std::mem::take(&mut dialog.remote_password)).filter(|p| !p.is_empty());
                if password.is_some()
                    || (dialog.remote_on, dialog.remote_port, dialog.remote_lan)
                        != (remote.enabled, remote.port, remote.lan)
                {
                    core.send(Command::SetRemote {
                        enabled: dialog.remote_on,
                        port: dialog.remote_port,
                        lan: dialog.remote_lan,
                        password,
                    });
                }
                if dialog.remote_https != remote.https {
                    core.send(Command::SetRemoteHttps(dialog.remote_https));
                }
                close = true;
            }
            if close || response.should_close() {
                self.settings = None;
            }
        }

        if let Some(dialog) = &mut self.name {
            let (mut close, mut save) = (false, false);
            let what = if dialog.tag { "tag" } else { "category" };
            let response = egui::Modal::new(egui::Id::unique("name")).show(ctx, |ui| {
                ui.set_width(320.0);
                ui.heading(format!("New {what}"));
                ui.add_space(4.0);
                let n = dialog.keys.len();
                ui.label(if n == 1 {
                    format!("Name of the {what} for this torrent")
                } else {
                    format!("Name of the {what} for these {n} torrents")
                });
                let r = ui.add(egui::TextEdit::singleline(&mut dialog.text).desired_width(f32::INFINITY));
                if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    save = true;
                } else if !r.has_focus() {
                    r.request_focus();
                }
                ui.add_space(6.0);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button(RichText::new("Save").strong()).clicked() {
                        save = true;
                    }
                    if ui.button("Cancel").clicked() {
                        close = true;
                    }
                });
            });
            let text = dialog.text.trim().to_owned();
            if save && !text.is_empty() {
                let keys = dialog.keys.clone();
                self.core.send(if dialog.tag {
                    Command::SetTag {
                        keys,
                        tag: text,
                        on: true,
                    }
                } else {
                    Command::SetCategory {
                        keys,
                        category: Some(text),
                    }
                });
                close = true;
            }
            if close || response.should_close() {
                self.name = None;
            }
        }

        if let Some(dialog) = &mut self.feeds {
            let (mut close, mut save) = (false, false);
            let core = &self.core;
            let snap = &self.snap;
            let response = egui::Modal::new(egui::Id::unique("feeds")).show(ctx, |ui| {
                ui.set_width(560.0);
                ui.heading("Feeds");
                ui.add_space(4.0);
                ui.label(
                    RichText::new(
                        "RSS and Atom feeds are read every 15 minutes. Every item whose title has all \
                         the “must have” words and none of the “must not have” words is downloaded, \
                         once. With no words, every item is.",
                    )
                    .weak(),
                );
                ui.add_space(6.0);
                let mut gone: Option<usize> = None;
                let mut bad = false;
                egui::ScrollArea::vertical().max_height(380.0).show(ui, |ui| {
                    if dialog.feeds.is_empty() {
                        ui.label(RichText::new("No feeds yet.").weak());
                    }
                    for (i, feed) in dialog.feeds.iter_mut().enumerate() {
                        ui.push_id(i, |ui| {
                            ui.group(|ui| {
                                ui.set_width(ui.available_width());
                                ui.horizontal(|ui| {
                                    ui.checkbox(&mut feed.enabled, "On");
                                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                        if ui.button("Remove").clicked() {
                                            gone = Some(i);
                                        }
                                        ui.add(
                                            egui::TextEdit::singleline(&mut feed.name)
                                                .hint_text("Name")
                                                .desired_width(f32::INFINITY),
                                        );
                                    });
                                });
                                egui::Grid::new("feed")
                                    .num_columns(2)
                                    .spacing([10.0, 4.0])
                                    .show(ui, |ui| {
                                        let line = |ui: &mut egui::Ui, label: &str, text: &mut String, hint: &str| {
                                            ui.label(RichText::new(label).weak());
                                            ui.add(
                                                egui::TextEdit::singleline(text)
                                                    .hint_text(hint)
                                                    .desired_width(f32::INFINITY),
                                            );
                                            ui.end_row();
                                        };
                                        line(ui, "Address", &mut feed.url, "https://…");
                                        line(ui, "Must have", &mut feed.include, "words, separated by spaces");
                                        line(ui, "Must not have", &mut feed.exclude, "words, separated by spaces");
                                        line(ui, "Category", &mut dialog.categories[i], "none");
                                        line(ui, "Episodes", &mut feed.episodes, "all, or like S02E05-");
                                    });
                                crate::feed_ui::options(ui, feed);
                                bad |= crate::feed_ui::problem(ui, feed);
                                let state = snap.feeds.iter().find(|f| f.feed.url == feed.url.trim());
                                let status = match state {
                                    None => RichText::new("Not saved yet").weak(),
                                    Some(f) => match (&f.error, f.checked_at) {
                                        (Some(e), _) => RichText::new(e).color(color(State::Error)),
                                        (None, 0) => RichText::new("Not read yet").weak(),
                                        (None, at) => RichText::new(format!(
                                            "Read {} ago",
                                            format::duration(now().saturating_sub(at))
                                        ))
                                        .weak(),
                                    },
                                };
                                ui.horizontal(|ui| {
                                    let url = feed.url.trim();
                                    if ui
                                        .add_enabled(!url.is_empty(), egui::Button::new("Preview").small())
                                        .clicked()
                                    {
                                        core.send(Command::PreviewFeed(Some(url.to_owned())));
                                    }
                                    ui.add(egui::Label::new(status.small()).truncate());
                                });
                            });
                        });
                    }
                });
                if let Some(i) = gone {
                    dialog.feeds.remove(i);
                    dialog.categories.remove(i);
                }
                if let Some(preview) = &snap.feed_preview {
                    let shown = dialog
                        .feeds
                        .iter()
                        .zip(&dialog.categories)
                        .find(|(f, _)| f.url.trim() == preview.url);
                    let feed = shown.map(|(f, c)| Feed {
                        category: Some(c.trim().to_owned()).filter(|c| !c.is_empty()),
                        ..f.clone()
                    });
                    ui.add_space(6.0);
                    if crate::feed_ui::preview(ui, core, preview, feed.as_ref()) {
                        core.send(Command::PreviewFeed(None));
                    }
                }
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if ui.button("+ Add feed").clicked() {
                        dialog.feeds.push(Feed::default());
                        dialog.categories.push(String::new());
                    }
                    if ui
                        .add_enabled(!snap.feeds.is_empty(), egui::Button::new("Read all now"))
                        .clicked()
                    {
                        core.send(Command::CheckFeeds);
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .add_enabled(!bad, egui::Button::new(RichText::new("Save").strong()))
                            .clicked()
                        {
                            save = true;
                        }
                        if ui.button("Cancel").clicked() {
                            close = true;
                        }
                    });
                });
            });
            if save {
                let feeds = dialog
                    .feeds
                    .iter()
                    .zip(&dialog.categories)
                    .map(|(feed, category)| Feed {
                        category: Some(category.trim().to_owned()).filter(|c| !c.is_empty()),
                        ..feed.clone()
                    })
                    .collect();
                core.send(Command::SetFeeds(feeds));
                close = true;
            }
            if close || response.should_close() {
                self.feeds = None;
                self.core.send(Command::PreviewFeed(None));
            }
        }
    }
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn peers(ui: &mut egui::Ui, t: &TorrentView) {
    if t.peers.is_empty() {
        ui.label(RichText::new("No connected peers.").weak());
        return;
    }
    TableBuilder::new(ui)
        .id_salt("peers")
        .striped(true)
        .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
        .column(Column::exact(220.0))
        .column(Column::remainder().at_least(120.0).clip(true))
        .column(Column::exact(60.0))
        .column(Column::exact(100.0))
        .column(Column::exact(100.0))
        .header(20.0, |mut h| {
            for name in ["Address", "Client", "Link", "Received", "Sent"] {
                h.col(|ui| {
                    ui.label(RichText::new(name).weak());
                });
            }
        })
        .body(|body| {
            body.rows(20.0, t.peers.len(), |mut row| {
                let p = &t.peers[row.index()];
                row.col(|ui| {
                    ui.label(&p.addr);
                });
                row.col(|ui| {
                    ui.add(egui::Label::new(&p.client).truncate());
                });
                row.col(|ui| {
                    ui.label(p.connection);
                });
                row.col(|ui| {
                    ui.label(format::bytes(p.downloaded));
                });
                row.col(|ui| {
                    ui.label(format::bytes(p.uploaded));
                });
            });
        });
}

fn trackers(ui: &mut egui::Ui, t: &TorrentView) {
    if t.trackers.is_empty() {
        ui.label(RichText::new("This torrent has no trackers. Peers are found through the DHT network.").weak());
        return;
    }
    egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
        for tracker in t.trackers.iter() {
            ui.label(tracker);
        }
    });
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();

        for text in crate::take_inbox() {
            self.core.send(Command::Add(text));
        }
        for file in ctx.input(|i| i.raw.dropped_files.clone()) {
            self.core.send(Command::Add(file.path().to_string_lossy().into_owned()));
        }

        let snap = self.core.snapshot();
        if !Arc::ptr_eq(&snap, &self.snap) {
            self.snap = snap;
            self.dirty = true;
        }
        if let Some(fatal) = self.snap.fatal.clone() {
            egui::CentralPanel::default().show(ui, |ui| {
                ui.add_space(40.0);
                ui.vertical_centered(|ui| {
                    ui.heading("RustTorrent could not start");
                    ui.add_space(8.0);
                    ui.label(fatal);
                });
            });
            return;
        }
        if self.dirty {
            self.rebuild();
        }

        #[cfg(feature = "shot")]
        {
            shot(&ctx);
            // Testing only: open the feeds window once the feeds have been read.
            {
                use std::sync::atomic::{AtomicBool, Ordering};
                static OPENED: AtomicBool = AtomicBool::new(false);
                let wanted = std::env::var("RUSTTORRENT_SHOT_TAB").unwrap_or_default();
                let known = wanted == "feeds" || wanted == "settings";
                if known && self.snap.ready && !OPENED.swap(true, Ordering::Relaxed) {
                    if wanted == "feeds" {
                        self.feeds = Some(FeedsDialog::of(&self.snap));
                    } else {
                        self.settings = Some(SettingsDialog::of(&self.snap.settings));
                    }
                }
            }
            // Testing only: open the first torrent's details on a given tab.
            if let (None, Ok(tab), Some(&first)) =
                (&self.focus, std::env::var("RUSTTORRENT_SHOT_TAB"), self.view.first())
            {
                let key = self.snap.torrents[first].key.clone();
                self.selected = vec![key.clone()];
                self.focus = Some(key);
                self.tab = match tab.as_str() {
                    "files" => Tab::Files,
                    "peers" => Tab::Peers,
                    "trackers" => Tab::Trackers,
                    _ => Tab::Overview,
                };
            }
        }

        let typing = ctx.egui_wants_keyboard_input();
        let no_dialog = self.add.is_none()
            && self.remove.is_none()
            && self.settings.is_none()
            && self.name.is_none()
            && self.feeds.is_none();
        if !typing
            && no_dialog
            && ctx.input(|i| i.key_pressed(egui::Key::Delete) || i.key_pressed(egui::Key::Backspace))
        {
            self.ask_remove(self.selected.clone());
        }

        egui::Panel::top("bar").show(ui, |ui| self.top_bar(ui));
        if let Some(why) = self.snap.blocked.clone().filter(|_| self.snap.ready) {
            egui::Panel::top("blocked").show(ui, |ui| {
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.label(RichText::new(why).strong().color(color(State::Blocked)));
                    if ui.button("Settings…").clicked() {
                        self.settings = Some(SettingsDialog::of(&self.snap.settings));
                    }
                });
                ui.add_space(4.0);
            });
        }
        egui::Panel::bottom("status").show(ui, |ui| self.status_bar(ui));
        egui::Panel::left("side")
            .resizable(false)
            .exact_size(170.0)
            .show(ui, |ui| self.sidebar(ui));
        egui::Panel::bottom("details")
            .resizable(true)
            .default_size(230.0)
            .min_size(140.0)
            .show(ui, |ui| self.details(ui));
        egui::CentralPanel::default().show(ui, |ui| self.table(ui));
        self.dialogs(&ctx);

        // Peers are only gathered for the torrent whose Peers tab is open.
        let want = if self.tab == Tab::Peers {
            self.focus.clone()
        } else {
            None
        };
        if want != self.watching {
            self.core.send(Command::Watch(want.clone()));
            self.watching = want;
        }
        if self.dirty {
            ctx.request_repaint();
        }
    }

    fn on_exit(&mut self) {
        self.core.shutdown();
    }
}

/// `RUSTTORRENT_SHOT=<file.png>` saves a picture of the window a few seconds after start.
#[cfg(feature = "shot")]
fn shot(ctx: &egui::Context) {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::OnceLock;
    static START: OnceLock<std::time::Instant> = OnceLock::new();
    static DONE: AtomicBool = AtomicBool::new(false);
    let Ok(path) = std::env::var("RUSTTORRENT_SHOT") else {
        return;
    };
    let wait: f32 = std::env::var("RUSTTORRENT_SHOT_AFTER")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(4.0);
    let elapsed = START.get_or_init(std::time::Instant::now).elapsed().as_secs_f32();
    if DONE.load(Ordering::Relaxed) {
        return;
    }
    if elapsed < wait {
        ctx.request_repaint_after(std::time::Duration::from_millis(100));
        return;
    }
    DONE.store(true, Ordering::Relaxed);
    ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::ScreenshotCallback::new(
        move |img| {
            let Ok(file) = std::fs::File::create(&path) else { return };
            let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), img.width() as u32, img.height() as u32);
            encoder.set_color(png::ColorType::Rgba);
            if let Ok(mut writer) = encoder.write_header() {
                let _ = writer.write_image_data(img.as_raw());
            }
        },
    )));
}
