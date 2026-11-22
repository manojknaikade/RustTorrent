//! The main window: toolbar, status filters, the torrent table, and details for the selected
//! torrent in the same window.
use std::sync::Arc;

use eframe::egui::{self, Color32, RichText, Sense};
use egui_extras::{Column, TableBuilder};
use rusttorrent_core::store::WatchFolder;
use rusttorrent_core::{
    format, import, manage, Command, Core, Feed, HoldReason, Move, Priority, Schedule, Snapshot, State, TorrentView,
};

mod limits;

fn color(state: State) -> Color32 {
    crate::comfort::ink(match state {
        State::Downloading => Color32::from_rgb(64, 156, 255),
        State::Seeding => Color32::from_rgb(52, 199, 123),
        State::Paused => Color32::from_rgb(150, 150, 158),
        State::Queued | State::Scheduled => Color32::from_rgb(170, 140, 235),
        State::FetchingInfo | State::Checking | State::Moving => Color32::from_rgb(230, 170, 60),
        State::Completed => Color32::from_rgb(120, 200, 190),
        State::Blocked | State::Error => Color32::from_rgb(240, 90, 90),
    })
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
    /// Watch folders, each with its category ("" for none).
    watch_dirs: Vec<(std::path::PathBuf, String)>,
    /// The one network interface to use, or `None` for any.
    interface: Option<String>,
    remote_on: bool,
    remote_port: u16,
    remote_lan: bool,
    /// A new password for remote control. Empty keeps the one already set.
    remote_password: String,
    /// The apps macOS opens magnet links and `.torrent` files with.
    #[cfg(target_os = "macos")]
    handlers: crate::mac::Handlers,
    /// Why making RustTorrent their default failed, if it did.
    #[cfg(target_os = "macos")]
    default_app_problem: Option<String>,
    ask_before_add: bool,
    /// Pause downloads when less than this many MiB is free. 0 is off.
    keep_free_mib: u64,
    network: crate::network::Form,
    comfort: rusttorrent_core::ComfortSettings,
    remote_https: bool,
}

/// Asks for the name of a new category or tag for some torrents.
struct NameDialog {
    keys: Vec<String>,
    tag: bool,
    text: String,
    /// For a category: the folder its torrents are saved to, if not the usual one.
    folder: Option<std::path::PathBuf>,
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

/// Every category made so far and every tag in use, each sorted by name.
fn names_in_use(snap: &Snapshot) -> (Vec<String>, Vec<String>) {
    let mut categories: Vec<String> = snap
        .torrents
        .iter()
        .filter_map(|t| t.category.clone())
        .chain(snap.feeds.iter().filter_map(|f| f.feed.category.clone()))
        .chain(snap.settings.categories.iter().cloned())
        .chain(snap.settings.category_dirs.keys().cloned())
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
            watch_dirs: (s.watch_dirs.iter())
                .map(|w| (w.dir.clone(), w.category.clone().unwrap_or_default()))
                .collect(),
            interface: s.bind_interface.clone(),
            remote_on: s.remote.enabled,
            remote_port: s.remote.port,
            remote_lan: s.remote.lan,
            remote_password: String::new(),
            #[cfg(target_os = "macos")]
            handlers: crate::mac::handlers(),
            #[cfg(target_os = "macos")]
            default_app_problem: None,
            ask_before_add: s.ask_before_add,
            keep_free_mib: s.keep_free_mib,
            network: crate::network::Form::of(&s.network),
            comfort: s.comfort.clone(),
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
    /// Torrents waiting in the add dialog.
    adding: crate::add_dialog::AddQueue,
    /// Asking whether to quit while downloads are running.
    quit: bool,
    /// The user agreed to quit.
    quit_ok: bool,
    limits: Option<limits::LimitsDialog>,
    create: Option<crate::make::CreateDialog>,
    extras: crate::comfort::Extras,
    /// Sidebar counts, worked out in `rebuild()` instead of every frame.
    side: Side,
    /// A file waiting for its stream address: torrent, file, copy instead of play, and
    /// when it was asked for.
    play: Option<(String, usize, bool, std::time::Instant)>,
    /// The tracker list being edited: torrent and the text, one tracker per line.
    tracker_edit: Option<(String, String)>,
    /// A torrent being renamed: its key, the name being typed and the torrent's own name.
    rename: Option<(String, String, String)>,
}

/// What the sidebar shows next to each status, category and tag.
#[derive(Default)]
struct Side {
    status: [usize; Filter::ALL.len()],
    categories: Vec<(String, usize)>,
    /// Only tags that some torrent has.
    tags: Vec<(String, usize)>,
}

impl Side {
    fn of(snap: &Snapshot) -> Self {
        let (categories, tags) = names_in_use(snap);
        let mut status = [0; Filter::ALL.len()];
        for (n, f) in status.iter_mut().zip(Filter::ALL) {
            *n = snap.torrents.iter().filter(|t| f.matches(t.state)).count();
        }
        let count = |name: &String, tag: bool| {
            snap.torrents
                .iter()
                .filter(|t| {
                    if tag {
                        t.tags.contains(name)
                    } else {
                        t.category.as_ref() == Some(name)
                    }
                })
                .count()
        };
        Self {
            status,
            categories: categories
                .into_iter()
                .map(|n| {
                    let c = count(&n, false);
                    (n, c)
                })
                .collect(),
            // A category stays until it is deleted; a tag goes when no torrent has it.
            tags: tags
                .into_iter()
                .map(|n| {
                    let c = count(&n, true);
                    (n, c)
                })
                .filter(|(_, c)| *c > 0)
                .collect(),
        }
    }
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

/// Opens a stream address in VLC when it is installed, or else in the default browser.
fn open_player(url: &str) {
    use std::process::Command as Run;
    let vlc = if cfg!(target_os = "macos") {
        std::path::Path::new("/Applications/VLC.app")
            .exists()
            .then(|| Run::new("open").args(["-a", "VLC", url]).spawn())
    } else if cfg!(windows) {
        let path = r"C:\Program Files\VideoLAN\VLC\vlc.exe";
        std::path::Path::new(path)
            .exists()
            .then(|| Run::new(path).arg(url).spawn())
    } else {
        Some(Run::new("vlc").arg(url).spawn())
    };
    if !matches!(vlc, Some(Ok(_))) {
        reveal(url);
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

pub(crate) const REVEAL: &str = if cfg!(target_os = "macos") {
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
            adding: Default::default(),
            quit: false,
            quit_ok: false,
            limits: None,
            create: None,
            extras: Default::default(),
            side: Side::default(),
            play: None,
            tracker_edit: None,
            rename: None,
        }
    }

    /// Adds these, through the add dialog unless the settings say not to ask.
    fn add_texts(&mut self, texts: Vec<String>) {
        for text in texts {
            if self.snap.settings.ask_before_add {
                self.adding.push(text);
            } else {
                self.core.send(Command::Add(text));
            }
        }
    }

    /// Something is downloading right now, so quitting would interrupt it.
    fn busy(&self) -> bool {
        self.snap
            .torrents
            .iter()
            .any(|t| matches!(t.state, State::Downloading | State::FetchingInfo | State::Checking))
    }

    fn rebuild(&mut self) {
        self.side = Side::of(&self.snap);
        let all = &self.snap.torrents;
        let q = self.search.to_lowercase();
        // A category that was deleted, or a tag that no torrent has any more, cannot be
        // filtered on.
        if self
            .category
            .as_ref()
            .is_some_and(|c| !self.side.categories.iter().any(|(n, _)| n == c))
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
        // Lower-case each name once, not twice per comparison.
        let lower: Vec<String> = if col == 2 {
            all.iter().map(|t| t.name.to_lowercase()).collect()
        } else {
            Vec::new()
        };
        self.view.sort_by(|&ia, &ib| {
            let (a, b) = (&all[ia], &all[ib]);
            let order = match col {
                1 => a.queue_pos.unwrap_or(u32::MAX).cmp(&b.queue_pos.unwrap_or(u32::MAX)),
                2 => lower[ia].cmp(&lower[ib]),
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
            if ui.button("Create torrent").clicked() {
                self.create = Some(crate::make::CreateDialog::new());
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
                crate::comfort::sparkline(ui, &mut self.extras, &self.snap);
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
                if limits::status(ui, &self.core, &self.snap) {
                    self.limits = Some(limits::LimitsDialog::of(&self.snap));
                }
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
        for (f, count) in Filter::ALL.into_iter().zip(self.side.status) {
            let text = format!("{}  {count}", f.label());
            let on = self.filter == f;
            let text = match f.dot() {
                Some(c) => {
                    RichText::new(format!("● {text}")).color(if on { ui.visuals().selection.stroke.color } else { c })
                }
                None => RichText::new(text),
            };
            if ui.selectable_label(on, text).clicked() {
                self.filter = f;
                self.dirty = true;
            }
        }
        let snap = self.snap.clone();
        let (categories, tags) = (self.side.categories.clone(), self.side.tags.clone());
        let (mut new_category, mut remove) = (false, None::<String>);
        let mut set_dir: Option<(String, Option<std::path::PathBuf>)> = None;
        egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            for (title, names, tag) in [("CATEGORIES", categories, false), ("TAGS", tags, true)] {
                if tag && names.is_empty() {
                    continue;
                }
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    ui.label(RichText::new(title).small().weak());
                    if !tag {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let add = ui.small_button("+").on_hover_text("New category");
                            crate::comfort::name(&add, egui::Role::Button, "New category");
                            if add.clicked() {
                                new_category = true;
                            }
                        });
                    }
                });
                if !tag && names.is_empty() {
                    ui.label(RichText::new("None yet. Click + to make one.").small().weak());
                }
                let chosen = if tag { &mut self.tag } else { &mut self.category };
                for (name, count) in names {
                    let on = chosen.as_ref() == Some(&name);
                    let label = egui::Button::selectable(on, format!("{name}  {count}")).truncate();
                    let r = ui.add(label);
                    // A second click shows everything again.
                    if r.clicked() {
                        *chosen = if on { None } else { Some(name.clone()) };
                        self.dirty = true;
                    }
                    if !tag {
                        let dir = snap.settings.category_dirs.get(&name);
                        let r = match dir {
                            Some(dir) => r.on_hover_text(format!("Saves to {}", dir.display())),
                            None => r,
                        };
                        r.context_menu(|ui| {
                            ui.menu_button("Set folder", |ui| {
                                let start = dir.unwrap_or(&snap.settings.download_dir);
                                if let Some(dir) = crate::folders::menu_items(ui, &snap.settings.recent_dirs, start) {
                                    set_dir = Some((name.clone(), Some(dir)));
                                }
                            });
                            if dir.is_some() && ui.button("Use the usual folder").clicked() {
                                set_dir = Some((name.clone(), None));
                                ui.close();
                            }
                            ui.separator();
                            if ui.button("Delete category").clicked() {
                                remove = Some(name.clone());
                                ui.close();
                            }
                        });
                    }
                }
            }
        });
        if new_category {
            self.name = Some(NameDialog {
                keys: Vec::new(),
                tag: false,
                text: String::new(),
                folder: None,
            });
        }
        if let Some(name) = remove {
            self.core.send(Command::RemoveCategory(name));
        }
        if let Some((category, dir)) = set_dir {
            self.core.send(Command::SetCategoryDir { category, dir });
        }
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
            Tab::Overview => {
                // Scrolls when the panel is too short for every line.
                egui::ScrollArea::vertical()
                    .auto_shrink(false)
                    .show(ui, |ui| self.overview(ui, t));
            }
            Tab::Files => self.files(ui, t),
            Tab::Peers => peers(ui, t, &self.core),
            Tab::Trackers => self.trackers(ui, t),
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
        if let Some(map) = self.snap.pieces.as_ref().filter(|m| m.key == t.key) {
            ui.add_space(2.0);
            crate::comfort::piece_strip(ui, map, color(State::Downloading));
        }
        ui.add_space(6.0);
        // Every column gets a fixed width from the panel's width, so changing numbers such
        // as the speeds never move the columns. Text too long for its column is cut short
        // and shown in full on hover.
        const KEY_W: f32 = 90.0;
        const GAP: f32 = 28.0;
        let value_w = ((ui.available_width() - 3.0 * KEY_W - 5.0 * GAP) / 3.0).clamp(110.0, 320.0);
        egui::Grid::new("overview")
            .num_columns(6)
            .spacing([GAP, 6.0])
            .show(ui, |ui| {
                let row = |ui: &mut egui::Ui, items: [(&str, String); 3]| {
                    for (k, v) in items {
                        let cell = |ui: &mut egui::Ui, w: f32, label: egui::Label| {
                            ui.allocate_ui_with_layout(
                                egui::vec2(w, ui.spacing().interact_size.y),
                                egui::Layout::left_to_right(egui::Align::Center),
                                |ui| {
                                    ui.set_width(w);
                                    ui.add(label.truncate());
                                },
                            );
                        };
                        cell(ui, KEY_W, egui::Label::new(RichText::new(k).weak()));
                        cell(ui, value_w, egui::Label::new(v));
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
                    Some(HoldReason::Queue) if t.queue_pos.is_none() => "A free seeding slot",
                    Some(HoldReason::Queue) => "A free download slot",
                    Some(HoldReason::Schedule) => "The schedule",
                    Some(HoldReason::Ratio) => limits::stopped_text(t),
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
                row(ui, limits::overview_row(t, &self.snap));
                if let Some(d) = self.snap.details.get(&t.key) {
                    row(ui, crate::make::detail_row(d));
                }
            });
        if let Some(c) = self.snap.details.get(&t.key).and_then(|d| d.comment.as_deref()) {
            ui.horizontal(|ui| {
                ui.label(RichText::new("Comment").weak());
                ui.add(egui::Label::new(c).truncate());
            });
        }
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
        if !t.is_pending() {
            self.torrent_limits(ui, t);
        }
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

    /// This torrent's own speed limits. They apply on top of the global ones.
    fn torrent_limits(&mut self, ui: &mut egui::Ui, t: &TorrentView) {
        let id = egui::Id::unique(("torrent-limits", &t.key));
        let saved = (t.down_limit_kib, t.up_limit_kib);
        // What is typed or dragged is kept here until it is sent and comes back.
        let mut value: (u32, u32) = ui.data(|d| d.get_temp(id)).unwrap_or(saved);
        let field = |ui: &mut egui::Ui, n: &mut u32| {
            ui.add(
                egui::DragValue::new(n)
                    .speed(10.0)
                    .range(0..=10_000_000)
                    .custom_formatter(|n, _| {
                        if n == 0.0 {
                            "No limit".to_owned()
                        } else {
                            format!("{n} KiB/s")
                        }
                    })
                    .custom_parser(|text| {
                        let text = text.trim().to_lowercase();
                        if text.is_empty() || text.starts_with("no") {
                            return Some(0.0);
                        }
                        text.trim_end_matches("kib/s").trim().parse::<f64>().ok()
                    }),
            )
        };
        let responses = ui
            .horizontal(|ui| {
                ui.label(RichText::new("Download limit").weak());
                let down = field(ui, &mut value.0);
                ui.add_space(20.0);
                ui.label(RichText::new("Upload limit").weak());
                let up = field(ui, &mut value.1);
                [down, up]
            })
            .inner;
        let busy = responses.iter().any(|r| r.dragged() || r.has_focus());
        let commit = responses
            .iter()
            .any(|r| r.drag_stopped() || r.lost_focus() || (r.changed() && !busy));
        if commit && value != saved {
            self.core.send(Command::SetTorrentLimits {
                keys: vec![t.key.clone()],
                down_kib: value.0,
                up_kib: value.1,
            });
        }
        if !busy && value == saved {
            ui.data_mut(|d| d.remove::<(u32, u32)>(id));
        } else {
            // Kept until the new limits come back from the engine, so the field does not jump.
            ui.data_mut(|d| d.insert_temp(id, value));
        }
    }

    fn files(&mut self, ui: &mut egui::Ui, t: &TorrentView) {
        let n = t.files.len();
        if n == 0 {
            ui.label(RichText::new("The file list has not arrived yet.").weak());
            return;
        }
        let is_on = |i: usize| t.selected.as_ref().is_none_or(|s| s.contains(&i));
        let mut change: Option<Vec<usize>> = None;
        let mut priority: Option<(usize, Priority)> = None;
        let mut play: Option<(usize, bool)> = None;
        ui.horizontal(|ui| {
            let chosen = t.selected.as_ref().map_or(n, |s| s.len());
            ui.label(RichText::new(format!("{chosen} of {n} files chosen for download")).weak());
            if chosen < n && ui.small_button("Choose all").clicked() {
                change = Some((0..n).collect());
            }
        });
        if n > 1 || t.done < t.total {
            ui.label(
                RichText::new(
                    "Downloads in order: high-priority files first, then normal, then low, by name within each, \
                     each file from its start to its end.",
                )
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
                        let now = t.priorities.get(i);
                        egui::ComboBox::from_id_salt(("priority", i))
                            .width(70.0)
                            .selected_text(now.label())
                            .show_ui(ui, |ui| {
                                for p in Priority::ALL {
                                    if ui.selectable_label(p == now, p.label()).clicked() && p != now {
                                        priority = Some((i, p));
                                    }
                                }
                            });
                        ui.add_sized(
                            [70.0, 18.0],
                            egui::Label::new(RichText::new(format::bytes(f.len)).weak()),
                        );
                        if ui
                            .small_button("Play")
                            .on_hover_text("Open in VLC, or else the browser, while it downloads")
                            .clicked()
                        {
                            play = Some((i, false));
                        }
                        if ui
                            .small_button("Copy link")
                            .on_hover_text("Copy an address any player on this computer can open")
                            .clicked()
                        {
                            play = Some((i, true));
                        }
                        ui.add(egui::Label::new(&f.name).truncate());
                    });
                }
            });
        if let Some((file, copy)) = play {
            let ready = self.snap.streams.iter().find(|s| s.key == t.key && s.file == file);
            match ready {
                Some(link) if copy => ui.ctx().copy_text(link.url.clone()),
                Some(link) => open_player(&link.url),
                None => {
                    self.core.send(Command::Play {
                        key: t.key.clone(),
                        file,
                    });
                    self.play = Some((t.key.clone(), file, copy, std::time::Instant::now()));
                }
            }
        }
        if let Some((file, priority)) = priority {
            self.core.send(Command::SetFilePriority {
                key: t.key.clone(),
                files: vec![file],
                priority,
            });
        }
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
        // Set when the menu asks to save a torrent's file: its key and name.
        let mut save_file: Option<(String, String)> = None;
        // Set when the menu asks to rename a torrent.
        let mut ask_rename: Option<(String, String, String)> = None;
        let mut move_to: Option<(Vec<String>, std::path::PathBuf)> = None;
        let recent = self.snap.settings.recent_dirs.clone();
        let (categories, tags) = names_in_use(&snap);
        // Columns the user hid with a right-click on the header.
        let hidden = &snap.settings.comfort.hidden_columns;
        let shown = heads.map(|name| !crate::comfort::is_hidden(hidden, name));
        let mut new_hidden: Option<Vec<String>> = None;
        let widths = [
            Column::exact(14.0),
            Column::exact(30.0),
            Column::remainder().at_least(200.0).clip(true),
            Column::exact(150.0),
            Column::exact(80.0),
            Column::exact(90.0),
            Column::exact(90.0),
            Column::exact(80.0),
            Column::exact(50.0),
            Column::exact(50.0),
        ];
        let mut builder = TableBuilder::new(ui)
            .striped(true)
            .sense(Sense::click())
            .cell_layout(egui::Layout::left_to_right(egui::Align::Center));
        for (column, on) in widths.into_iter().zip(shown) {
            if on {
                builder = builder.column(column);
            }
        }
        builder
            .header(24.0, |mut h| {
                for (i, name) in heads.iter().enumerate() {
                    if !shown[i] {
                        continue;
                    }
                    let (_, cell) = h.col(|ui| {
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
                        let label = ui.add(label);
                        if label.clicked() && i > 0 {
                            sort = if sort.0 == i { (i, !sort.1) } else { (i, i <= 2) };
                        }
                        label.context_menu(|ui| {
                            if let Some(list) = crate::comfort::columns_menu(ui, hidden) {
                                new_hidden = Some(list);
                            }
                        });
                    });
                    cell.context_menu(|ui| {
                        if let Some(list) = crate::comfort::columns_menu(ui, hidden) {
                            new_hidden = Some(list);
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
                        let dot = ui.label(RichText::new("●").color(color(t.state)));
                        crate::comfort::name(&dot, egui::Role::Label, t.state.label());
                    });
                    if shown[1] {
                        row.col(|ui| {
                            if let Some(p) = t.queue_pos {
                                ui.label(RichText::new(p.to_string()).weak());
                            }
                        });
                    }
                    row.col(|ui| {
                        ui.add(egui::Label::new(&t.name).truncate().selectable(false));
                    });
                    if shown[3] {
                        row.col(|ui| bar(ui, t));
                    }
                    if shown[4] {
                        row.col(|ui| {
                            ui.label(if t.total == 0 {
                                "–".to_owned()
                            } else {
                                format::bytes(t.total)
                            });
                        });
                    }
                    if shown[5] {
                        row.col(|ui| {
                            ui.label(format::speed(t.down_bps));
                        });
                    }
                    if shown[6] {
                        row.col(|ui| {
                            ui.label(format::speed(t.up_bps));
                        });
                    }
                    if shown[7] {
                        row.col(|ui| {
                            ui.label(format::eta(t.total.saturating_sub(t.done), t.down_bps));
                        });
                    }
                    if shown[8] {
                        row.col(|ui| {
                            ui.label(t.peers_live.to_string());
                        });
                    }
                    if shown[9] {
                        row.col(|ui| {
                            ui.label(format::ratio(t.uploaded, t.done));
                        });
                    }
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
                        if key.len() == 1
                            && !t.is_pending()
                            && ui
                                .button("Rename…")
                                .on_hover_text("Show it under another name. Its files keep their names.")
                                .clicked()
                        {
                            ask_rename = Some((t.key.clone(), t.name.clone(), t.own_name.clone()));
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
                        if let Some(command) = limits::seeding_menu(ui, t, &key) {
                            action = Some((command, false));
                        }
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
                        let mut forced = t.force_start;
                        if ui
                            .add_enabled(!t.is_pending(), egui::Checkbox::new(&mut forced, "Force start"))
                            .on_hover_text(
                                "Download even when the queue is full. The schedule and the network \
                                 interface still apply.",
                            )
                            .clicked()
                        {
                            action = Some((
                                Command::SetForceStart {
                                    keys: key.clone(),
                                    on: forced,
                                },
                                false,
                            ));
                            ui.close();
                        }
                        ui.add_enabled_ui(!t.is_pending(), |ui| {
                            ui.menu_button("Copy", |ui| {
                                let chosen = || all.iter().filter(|t| key.contains(&t.key) && !t.is_pending());
                                let mut copied: Option<String> = None;
                                if ui.button("Magnet link").clicked() {
                                    copied = Some(chosen().map(manage::magnet_link).collect::<Vec<_>>().join("\n"));
                                }
                                if ui.button("Info-hash").clicked() {
                                    copied = Some(chosen().map(|t| t.key.clone()).collect::<Vec<_>>().join("\n"));
                                }
                                if ui.button("Name").clicked() {
                                    copied = Some(chosen().map(|t| t.name.clone()).collect::<Vec<_>>().join("\n"));
                                }
                                if let Some(text) = copied {
                                    ui.ctx().copy_text(text);
                                    ui.close();
                                }
                            });
                        });
                        ui.separator();
                        let live = !t.is_pending() && t.state != State::Moving;
                        if ui
                            .add_enabled(live, egui::Button::new("Recheck"))
                            .on_hover_text("Check the data on disk again and download what is missing or wrong.")
                            .clicked()
                        {
                            action = Some((Command::Recheck(key.clone()), false));
                            ui.close();
                        }
                        if ui
                            .add_enabled(live, egui::Button::new("Reannounce"))
                            .on_hover_text("Ask the trackers for peers now, without waiting for the next time.")
                            .clicked()
                        {
                            action = Some((Command::Reannounce(key.clone()), false));
                            ui.close();
                        }
                        ui.add_enabled_ui(live, |ui| {
                            ui.menu_button("Move data to", |ui| {
                                let start = std::path::Path::new(&t.save_path);
                                if let Some(dir) = crate::folders::menu_items(ui, &recent, start) {
                                    move_to = Some((key.clone(), dir));
                                }
                            })
                            .response
                            .on_hover_text("Move the downloaded files to another folder.");
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
                        ui.separator();
                        // A link's file can be saved once its details have arrived.
                        let known = snap.details.contains_key(&t.key);
                        if ui
                            .add_enabled(known, egui::Button::new("Save .torrent file…"))
                            .on_disabled_hover_text("The details of this torrent have not arrived yet.")
                            .clicked()
                        {
                            save_file = Some((t.key.clone(), t.name.clone()));
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
        if ask_rename.is_some() {
            self.rename = ask_rename;
        }
        if let Some((keys, tag)) = ask_name {
            self.name = Some(NameDialog {
                keys,
                tag,
                text: String::new(),
                folder: None,
            });
        }
        if sort != self.sort {
            self.sort = sort;
            self.dirty = true;
        }
        if let Some((keys, dir)) = move_to {
            self.core.send(Command::MoveData { keys, dir });
        }
        if let Some((key, name)) = save_file {
            let picked = rfd::FileDialog::new()
                .add_filter("Torrent files", &["torrent"])
                .set_file_name(format!("{name}.torrent"))
                .save_file();
            if let Some(path) = picked {
                self.core.send(Command::SaveTorrentFile { key, path });
            }
        }
        if let Some(hidden_columns) = new_hidden {
            let mut comfort = snap.settings.comfort.clone();
            comfort.hidden_columns = hidden_columns;
            self.core.send(Command::SetComfort(comfort));
        }
    }

    fn dialogs(&mut self, ctx: &egui::Context) {
        let mut texts: Vec<String> = Vec::new();
        if let Some(dialog) = &mut self.create {
            if dialog.show(ctx, &self.core, &self.snap) {
                self.create = None;
            }
        }

        if let Some(text) = &mut self.add {
            let (mut close, mut submit) = (false, false);
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
                                texts.push(f.to_string_lossy().into_owned());
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
                    texts.push(line.to_owned());
                }
                close = true;
            }
            if close || response.should_close() {
                self.add = None;
            }
        }
        self.add_texts(texts);
        if self.add.is_none() && self.remove.is_none() && self.settings.is_none() {
            let categories = names_in_use(&self.snap).0;
            self.adding.show(ctx, &self.core, &self.snap, &categories);
        }

        if self.quit {
            let (mut close, mut quit) = (false, false);
            let response = egui::Modal::new(egui::Id::unique("quit")).show(ctx, |ui| {
                ui.set_width(380.0);
                ui.heading("Quit RustTorrent?");
                ui.add_space(4.0);
                ui.label("Some torrents are still downloading. They will go on from where they stopped the next time the app opens.");
                ui.add_space(8.0);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button(RichText::new("Quit").strong()).clicked() {
                        quit = true;
                    }
                    if ui.button("Cancel").clicked() {
                        close = true;
                    }
                });
            });
            if quit {
                self.quit_ok = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            if quit || close || response.should_close() {
                self.quit = false;
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

        let (mut open_stats, mut open_log) = (false, false);
        if let Some(dialog) = &mut self.settings {
            let (mut close, mut save) = (false, false);
            let core = &self.core;
            let snap = &self.snap;
            // The body scrolls so the heading and the buttons stay on screen in a short window.
            let tall = (ctx.content_rect().height() - 140.0).max(200.0);
            let response = egui::Modal::new(egui::Id::unique("settings")).show(ctx, |ui| {
                ui.set_width(460.0);
                ui.heading("Settings");
                ui.add_space(6.0);
                let problem = egui::ScrollArea::vertical().max_height(tall).show(ui, |ui| {
                ui.label(RichText::new("New torrents are saved to").weak());
                ui.horizontal(|ui| {
                    let start = &snap.settings.download_dir;
                    if let Some(dir) = crate::folders::button(ui, "Change", &snap.settings.recent_dirs, start) {
                        core.send(Command::SetDownloadDir(dir));
                    }
                    ui.add(egui::Label::new(snap.settings.download_dir.display().to_string()).truncate());
                });
                ui.label(RichText::new("Unfinished downloads are kept in").weak())
                    .on_hover_text("Each torrent is moved to its own folder once it finishes.");
                ui.horizontal(|ui| {
                    let incomplete = snap.settings.incomplete_dir.as_ref();
                    let start = incomplete.unwrap_or(&snap.settings.download_dir);
                    if let Some(dir) = crate::folders::button(ui, "Change", &snap.settings.recent_dirs, start) {
                        core.send(Command::SetIncompleteDir(Some(dir)));
                    }
                    match incomplete {
                        Some(dir) => {
                            if ui.button("Clear").clicked() {
                                core.send(Command::SetIncompleteDir(None));
                            }
                            ui.add(egui::Label::new(dir.display().to_string()).truncate());
                        }
                        None => {
                            ui.label(RichText::new("The same folder").weak());
                        }
                    }
                });
                ui.checkbox(
                    &mut dialog.ask_before_add,
                    "Ask where to save and which files to download when adding",
                )
                .on_hover_text("Torrents from the watch folder, feeds and the remote page are added without asking.");
                ui.horizontal(|ui| {
                    let mut on = dialog.keep_free_mib > 0;
                    if ui
                        .checkbox(&mut on, "Pause downloads when the disk has less than")
                        .on_hover_text("Checked every 10 seconds. Resume them by hand once there is room.")
                        .changed()
                    {
                        dialog.keep_free_mib = if on { 1024 } else { 0 };
                    }
                    let mut gib = dialog.keep_free_mib as f64 / 1024.0;
                    let drag = egui::DragValue::new(&mut gib).range(0.1..=1000.0).speed(0.1).suffix(" GB").max_decimals(1);
                    if ui.add_enabled(on, drag).changed() {
                        dialog.keep_free_mib = ((gib * 1024.0).round() as u64).max(1);
                    }
                    ui.label("free");
                });
                egui::CollapsingHeader::new("Folder per category")
                    .id_salt("category-dirs")
                    .show(ui, |ui| {
                        let mut names = names_in_use(snap).0;
                        for name in snap.settings.category_dirs.keys() {
                            if !names.contains(name) {
                                names.push(name.clone());
                            }
                        }
                        if names.is_empty() {
                            ui.label(RichText::new("No categories yet.").weak());
                        }
                        egui::Grid::new("category-dirs-grid")
                            .num_columns(3)
                            .spacing([12.0, 4.0])
                            .show(ui, |ui| {
                                for name in names {
                                    let dir = snap.settings.category_dirs.get(&name);
                                    ui.label(&name);
                                    let text = match dir {
                                        Some(dir) => RichText::new(dir.display().to_string()),
                                        None => RichText::new("The folder above").weak(),
                                    };
                                    ui.add(egui::Label::new(text).truncate());
                                    ui.horizontal(|ui| {
                                        let start = dir.unwrap_or(&snap.settings.download_dir);
                                        let recent = &snap.settings.recent_dirs;
                                        if let Some(dir) = crate::folders::button(ui, "Change", recent, start) {
                                            core.send(Command::SetCategoryDir {
                                                category: name.clone(),
                                                dir: Some(dir),
                                            });
                                        }
                                        if dir.is_some() && ui.button("Clear").clicked() {
                                            core.send(Command::SetCategoryDir {
                                                category: name.clone(),
                                                dir: None,
                                            });
                                        }
                                    });
                                    ui.end_row();
                                }
                            });
                        ui.label(
                            RichText::new(
                                "Used when a torrent is added, and when a torrent is put in the category: its data moves there. Changing a folder here does not move torrents already in the category.",
                            )
                            .small()
                            .weak(),
                        );
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
                ui.label(RichText::new("Add .torrent files put in these folders").weak());
                let mut gone = None;
                for (i, (dir, category)) in dialog.watch_dirs.iter_mut().enumerate() {
                    ui.horizontal(|ui| {
                        let stop = ui.small_button("✕").on_hover_text("Stop watching this folder");
                        crate::comfort::name(&stop, egui::Role::Button, "Stop watching this folder");
                        if stop.clicked() {
                            gone = Some(i);
                        }
                        ui.add(
                            egui::TextEdit::singleline(category)
                                .hint_text("no category")
                                .desired_width(90.0),
                        )
                        .on_hover_text("The category the torrents from this folder get");
                        ui.add(egui::Label::new(dir.display().to_string()).truncate());
                    });
                }
                if let Some(i) = gone {
                    dialog.watch_dirs.remove(i);
                }
                ui.horizontal(|ui| {
                    if ui.button("Add folder…").clicked() {
                        if let Some(dir) = rfd::FileDialog::new().pick_folder() {
                            if !dialog.watch_dirs.iter().any(|(d, _)| *d == dir) {
                                dialog.watch_dirs.push((dir, String::new()));
                            }
                        }
                    }
                    ui.label(
                        RichText::new("A file that has been added is renamed to end in “.added”.")
                            .small()
                            .weak(),
                    );
                });
                ui.add_space(8.0);
                ui.label(RichText::new("Take over the torrents of another app").weak()).on_hover_text(
                    "Quit the other app first. Its data stays where it is and is checked before \
                     RustTorrent goes on. Its own list is not changed.",
                );
                ui.horizontal(|ui| {
                    for client in [import::Client::Transmission, import::Client::QBittorrent] {
                        if ui.button(format!("Import from {}…", client.name())).clicked() {
                            let mut pick = rfd::FileDialog::new().set_title(match client {
                                import::Client::Transmission => "Choose Transmission's folder",
                                import::Client::QBittorrent => "Choose qBittorrent's BT_backup folder",
                            });
                            if let Some(dir) = client.default_dir().filter(|d| d.is_dir()) {
                                pick = pick.set_directory(dir);
                            }
                            if let Some(dir) = pick.pick_folder() {
                                core.send(Command::Import(client, dir));
                            }
                        }
                    }
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
                crate::network::section(
                    ui,
                    &mut dialog.network,
                    dialog.interface.as_deref(),
                    dialog.remote_on,
                    snap,
                );
                let problem = problem.or(dialog.network.problem(dialog.interface.as_deref()));
                #[cfg(target_os = "macos")]
                {
                    ui.add_space(8.0);
                    ui.separator();
                    let ours = dialog.handlers.ours();
                    ui.horizontal_wrapped(|ui| {
                        ui.label(dialog.handlers.describe());
                        if !ours && ui.button("Open them in RustTorrent").clicked() {
                            dialog.default_app_problem = crate::mac::make_default().err();
                            dialog.handlers = crate::mac::handlers();
                        }
                    });
                    if let Some(problem) = &dialog.default_app_problem {
                        ui.colored_label(ui.visuals().warn_fg_color, problem);
                    }
                    ui.checkbox(
                        &mut dialog.comfort.keep_default_app,
                        "Keep it that way: take them back when another app, such as qBittorrent, makes itself the default",
                    );
                }
                ui.add_space(8.0);
                ui.separator();
                // Folded so the dialog still fits on a small screen.
                egui::CollapsingHeader::new("Finished downloads, look and statistics")
                    .default_open(crate::comfort::shot_theme().is_some())
                    .show(ui, |ui| {
                        (open_stats, open_log) = crate::comfort::settings_section(ui, &mut dialog.comfort);
                    });
                ui.add_space(8.0);
                ui.label(
                    RichText::new(format!("RustTorrent {}", rusttorrent_core::VERSION))
                        .small()
                        .weak(),
                );
                problem
                }).inner;
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
                let watch: Vec<WatchFolder> = (dialog.watch_dirs.iter())
                    .map(|(dir, category)| WatchFolder {
                        dir: dir.clone(),
                        category: Some(category.trim().to_owned()).filter(|c| !c.is_empty()),
                    })
                    .collect();
                if watch != snap.settings.watch_dirs {
                    core.send(Command::SetWatchDirs(watch));
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
                if dialog.ask_before_add != snap.settings.ask_before_add {
                    core.send(Command::SetAskBeforeAdd(dialog.ask_before_add));
                }
                if dialog.keep_free_mib != snap.settings.keep_free_mib {
                    core.send(Command::SetKeepFree(dialog.keep_free_mib));
                }
                crate::network::save(core, &dialog.network, snap);
                if dialog.comfort != snap.settings.comfort {
                    core.send(Command::SetComfort(dialog.comfort.clone()));
                }
                if dialog.remote_https != remote.https {
                    core.send(Command::SetRemoteHttps(dialog.remote_https));
                }
                close = true;
            }
            if close || response.should_close() || open_stats || open_log {
                self.settings = None;
            }
        }
        if open_stats {
            self.extras.stats = true;
        }
        if open_log {
            self.extras.open_log();
        }
        self.extras.windows(ctx, &self.snap);

        if let Some((key, text, own)) = &mut self.rename {
            let (mut close, mut save) = (false, false);
            let response = egui::Modal::new(egui::Id::unique("rename")).show(ctx, |ui| {
                ui.set_width(360.0);
                ui.heading("Rename");
                ui.add_space(4.0);
                ui.label("The name the list shows. The files on disk keep their names.");
                let r = ui.add(egui::TextEdit::singleline(text).desired_width(f32::INFINITY));
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
                    if !own.is_empty() && ui.button("Use its own name").on_hover_text(own.clone()).clicked() {
                        text.clear();
                        save = true;
                    }
                });
            });
            if save {
                // The torrent's own name typed back means no name of the user's.
                let name = if text.trim() == own.as_str() {
                    String::new()
                } else {
                    text.clone()
                };
                self.core.send(Command::Rename { key: key.clone(), name });
            }
            if save || close || response.should_close() {
                self.rename = None;
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
                ui.label(if n == 0 {
                    format!("Name of the new {what}")
                } else if n == 1 {
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
                if !dialog.tag {
                    ui.add_space(6.0);
                    ui.label("Save its torrents to");
                    ui.horizontal(|ui| {
                        let start = dialog.folder.as_ref().unwrap_or(&self.snap.settings.download_dir);
                        let recent = &self.snap.settings.recent_dirs;
                        if let Some(dir) = crate::folders::button(ui, "Choose", recent, start) {
                            dialog.folder = Some(dir);
                        }
                        if dialog.folder.is_some() && ui.button("Default").clicked() {
                            dialog.folder = None;
                        }
                        let text = match &dialog.folder {
                            Some(dir) => RichText::new(dir.display().to_string()),
                            None => RichText::new("The usual download folder").weak(),
                        };
                        ui.add(egui::Label::new(text).truncate());
                    });
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
                // The folder is set first, so torrents put in the new category move there.
                if let (false, Some(dir)) = (dialog.tag, dialog.folder.clone()) {
                    self.core.send(Command::SetCategoryDir {
                        category: text.clone(),
                        dir: Some(dir),
                    });
                }
                self.core.send(if keys.is_empty() && !dialog.tag {
                    Command::AddCategory(text)
                } else if dialog.tag {
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

        if let Some(dialog) = &mut self.limits {
            if dialog.show(ctx, &self.core) {
                self.limits = None;
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

fn peers(ui: &mut egui::Ui, t: &TorrentView, core: &Core) {
    if t.peers.is_empty() {
        ui.label(RichText::new("No connected peers.").weak());
        return;
    }
    let height = ui.available_height();
    TableBuilder::new(ui)
        .id_salt("peers")
        .striped(true)
        .auto_shrink(false)
        .max_scroll_height(height)
        .sense(Sense::click())
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
                row.response()
                    .context_menu(|ui| crate::network::peer_menu(ui, core, &p.addr));
            });
        });
}

impl App {
    fn trackers(&mut self, ui: &mut egui::Ui, t: &TorrentView) {
        let live = !t.is_pending() && t.state != State::Moving;
        if let Some((key, text)) = self.tracker_edit.as_mut().filter(|(k, _)| k == &t.key) {
            ui.label(RichText::new("One tracker address per line. Saving announces to the new list.").weak());
            egui::ScrollArea::vertical()
                .max_height((ui.available_height() - 36.0).max(60.0))
                .show(ui, |ui| {
                    ui.add(
                        egui::TextEdit::multiline(text)
                            .code_editor()
                            .desired_width(f32::INFINITY)
                            .desired_rows(6)
                            .hint_text("udp://tracker.example.org:1337/announce"),
                    );
                });
            let mut done = false;
            ui.horizontal(|ui| {
                if ui.add_enabled(live, egui::Button::new("Save")).clicked() {
                    let trackers = text
                        .lines()
                        .map(str::trim)
                        .filter(|l| !l.is_empty())
                        .map(String::from)
                        .collect();
                    self.core.send(Command::SetTrackers {
                        key: key.clone(),
                        trackers,
                    });
                    done = true;
                } else if ui.button("Cancel").clicked() {
                    done = true;
                }
            });
            if done {
                self.tracker_edit = None;
            }
            return;
        }
        if ui
            .add_enabled(live, egui::Button::new("Edit trackers…"))
            .on_hover_text("Add, remove or change this torrent's trackers.")
            .clicked()
        {
            self.tracker_edit = Some((t.key.clone(), t.trackers.join("\n")));
        }
        ui.add_space(4.0);
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
}

impl App {
    fn draw(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();

        let snap = self.core.snapshot();
        if !Arc::ptr_eq(&snap, &self.snap) {
            self.snap = snap;
            self.dirty = true;
        }
        if let Some((key, file, copy, at)) = &self.play {
            let link = self.snap.streams.iter().find(|s| &s.key == key && s.file == *file);
            if let Some(link) = link {
                if *copy {
                    ctx.copy_text(link.url.clone());
                } else {
                    open_player(&link.url);
                }
                self.play = None;
            } else if at.elapsed() > std::time::Duration::from_secs(10) {
                // The engine said why in a notice.
                self.play = None;
            }
        }

        // Wait for the saved settings, which say whether to ask before adding.
        if self.snap.ready || self.snap.fatal.is_some() {
            let mut texts = crate::take_inbox();
            texts.extend(
                ctx.input(|i| i.raw.dropped_files.clone())
                    .iter()
                    .map(|f| f.path().to_string_lossy().into_owned()),
            );
            self.add_texts(texts);
        }

        // Ask before quitting while something downloads.
        let busy = self.busy();
        #[cfg(target_os = "macos")]
        {
            crate::mac::ASK_BEFORE_QUIT.store(busy && !self.quit_ok, std::sync::atomic::Ordering::Relaxed);
            if crate::mac::QUIT_ASKED.swap(false, std::sync::atomic::Ordering::Relaxed) {
                self.quit = true;
            }
        }
        let close = ctx.input(|i| i.viewport().close_requested());
        #[cfg(target_os = "macos")]
        let close = if close && !self.quit_ok && !self.extras.closing() && crate::menu_bar::hides_on_close(&self.snap) {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            crate::menu_bar::hide(&ctx);
            false
        } else {
            close
        };
        if close && busy && !self.quit_ok {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.quit = true;
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
                let known = ["feeds", "settings", "add", "quit", "limits", "stats", "log"].contains(&wanted.as_str())
                    || wanted.starts_with("create");
                if known && self.snap.ready && !OPENED.swap(true, Ordering::Relaxed) {
                    if wanted == "limits" {
                        self.limits = Some(limits::LimitsDialog::of(&self.snap));
                    } else if wanted.starts_with("create") {
                        // "create" shows the window with RUSTTORRENT_SHOT_SOURCE chosen;
                        // "creating" also starts the work.
                        let source = std::env::var_os("RUSTTORRENT_SHOT_SOURCE").map(std::path::PathBuf::from);
                        let mut dialog = match source {
                            Some(s) => crate::make::CreateDialog::with_source(s),
                            None => crate::make::CreateDialog::new(),
                        };
                        if wanted == "creating" {
                            dialog.start(&self.core, &self.snap);
                        }
                        self.create = Some(dialog);
                    } else if wanted == "feeds" {
                        self.feeds = Some(FeedsDialog::of(&self.snap));
                    } else if wanted == "add" {
                        // RUSTTORRENT_SHOT_ADD holds the link or file to show in the add dialog.
                        let text = std::env::var("RUSTTORRENT_SHOT_ADD").unwrap_or_default();
                        self.adding.push(text);
                    } else if wanted == "quit" {
                        self.quit = true;
                    } else if wanted == "stats" {
                        self.extras.stats = true;
                    } else if wanted == "log" {
                        self.extras.open_log();
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
            && self.feeds.is_none()
            && !self.adding.is_open()
            && !self.quit
            && self.limits.is_none()
            && self.create.is_none();
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
            .show(ui, |ui| {
                // The content gets exactly the height the user set, so a long list or a
                // tab switch never makes the panel grow. Anything taller scrolls or is cut.
                let rect = ui.available_rect_before_wrap();
                let mut inner = ui.new_child(egui::UiBuilder::new().max_rect(rect).layout(*ui.layout()));
                inner.shrink_clip_rect(rect);
                self.details(&mut inner);
                ui.allocate_rect(rect, Sense::hover());
            });
        egui::CentralPanel::default().show(ui, |ui| self.table(ui));
        self.dialogs(&ctx);

        // Peers and pieces are only gathered for the torrent whose Overview or Peers tab is open.
        let want = if matches!(self.tab, Tab::Peers | Tab::Overview) {
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
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.draw(ui);
        #[cfg(target_os = "macos")]
        crate::mac::copy_out(ui.ctx());
    }

    #[cfg(target_os = "macos")]
    fn raw_input_hook(&mut self, _ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        crate::mac::clipboard_keys(&mut raw_input.events);
    }

    fn on_exit(&mut self) {
        self.core.shutdown();
    }

    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let snap = self.core.snapshot();
        self.extras.logic(ctx, &snap);
        #[cfg(target_os = "macos")]
        crate::menu_bar::logic(ctx, &snap, &self.core);
        #[cfg(target_os = "macos")]
        crate::mac::KEEP_DEFAULT.store(
            snap.settings.comfort.keep_default_app,
            std::sync::atomic::Ordering::Relaxed,
        );
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
    // Keep drawing: the picture is taken on the frame after it is asked for.
    if DONE.load(Ordering::Relaxed) {
        ctx.request_repaint_after(std::time::Duration::from_millis(100));
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
