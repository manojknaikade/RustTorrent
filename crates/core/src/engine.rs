//! The only file that talks to librqbit.
//!
//! One worker owns the engine session and the store. It takes commands, and once a second
//! it compares what each torrent is doing with what the arbiter says it should be doing,
//! fixes any difference, and publishes a snapshot if anything changed.
//!
//! The engine session ties every socket to one network interface when it starts. So when
//! the user has chosen an interface, the session only exists while that interface is
//! connected: the worker stops it when the interface goes away and starts a new one when
//! it is back. While there is no session nothing is sent or received.
use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::num::NonZeroU32;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use librqbit::api::TorrentIdOrHash;
use librqbit::limits::LimitsConfig;
use librqbit::{
    AddTorrent, AddTorrentOptions, AddTorrentResponse, ManagedTorrent, Session, SessionOptions,
    SessionPersistenceConfig, TorrentStatsState,
};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

use chrono::{Datelike, Timelike};

use crate::arbiter::{decide, Decision, HoldReason, Intent};
use crate::feeds::{self, Feed};
use crate::intake::{self, Source};
use crate::limits::{LimitSettings, TorrentLimits};
use crate::manage::{self, AddOptions, Inspector, Prepared};
use crate::model::{FeedView, FileMeta, Notice, PeerView, Snapshot, State, TorrentView};
use crate::net::{self, Interface};
use crate::rules::{self, Clock, Move, Schedule};
use crate::store::{Record, Store};
use crate::{now_secs, Shared};

mod feed_preview;
mod upload;

#[derive(Debug, Clone)]
pub enum Command {
    /// A magnet link, web address, file address or file path. See [`crate::intake`].
    Add(String),
    Pause(Vec<String>),
    Resume(Vec<String>),
    Remove {
        keys: Vec<String>,
        delete_files: bool,
    },
    /// Choose which files of a torrent to download.
    SetFiles {
        key: String,
        files: Vec<usize>,
    },
    /// Limits in KiB/s. 0 means no limit.
    SetLimits {
        down_kib: u32,
        up_kib: u32,
    },
    SetDownloadDir(PathBuf),
    /// How many torrents download at once, the ratio limit in percent and the schedule.
    /// 0 means no limit.
    SetRules {
        max_downloads: u32,
        ratio_limit_pct: u32,
        schedule: Schedule,
    },
    /// A ratio limit in percent for these torrents alone. `None` uses the one in the
    /// settings; 0 means no limit.
    SetRatioLimit {
        keys: Vec<String>,
        limit: Option<u32>,
    },
    MoveInQueue {
        keys: Vec<String>,
        how: Move,
    },
    /// Put these torrents in a category, or in none.
    SetCategory {
        keys: Vec<String>,
        category: Option<String>,
    },
    /// Give these torrents a tag, or take it away.
    SetTag {
        keys: Vec<String>,
        tag: String,
        on: bool,
    },
    /// The one network interface all traffic must use, or `None` for any.
    SetInterface(Option<String>),
    /// Remote control from a browser. A password is needed before it can be turned on;
    /// `None` keeps the one already set.
    SetRemote {
        enabled: bool,
        port: u16,
        lan: bool,
        password: Option<String>,
    },
    /// The folder to watch for `.torrent` files, or none.
    SetWatchDir(Option<PathBuf>),
    /// Replaces the list of feeds.
    SetFeeds(Vec<Feed>),
    /// Read every feed now instead of waiting for its turn.
    CheckFeeds,
    /// The torrent whose peers the front end wants to see.
    Watch(Option<String>),
    /// Fetch the name and files of these torrents for the add dialog, and stop fetching
    /// any others. An empty list stops it all.
    Inspect(Vec<String>),
    /// Add with the choices made in the add dialog.
    AddWith(AddOptions),
    /// Let these torrents download even when the queue is full, or not.
    SetForceStart {
        keys: Vec<String>,
        on: bool,
    },
    /// The folder a category saves to, or `None` for the download folder.
    SetCategoryDir {
        category: String,
        dir: Option<PathBuf>,
    },
    /// Makes a category that torrents can be put in later.
    AddCategory(String),
    /// Deletes a category: torrents in it are left with none, and its folder is forgotten.
    /// Files stay where they are.
    RemoveCategory(String),
    /// Whether adding from the window shows the add dialog first.
    SetAskBeforeAdd(bool),
    Shutdown,
    /// Turn slow mode on or off by hand.
    SetSlowMode(bool),
    /// Slow mode's limits and schedule, seeding time limits, queue limits and peers per
    /// torrent. Whether slow mode is on by hand is left as it is.
    SetLimitSettings(LimitSettings),
    /// Seeding time limits in minutes for these torrents alone. `None` uses the ones in
    /// the settings; 0 means no limit.
    SetSeedingLimits {
        keys: Vec<String>,
        time_limit_min: Option<u32>,
        idle_limit_min: Option<u32>,
    },
    /// Replaces the network settings. The engine restarts to apply them.
    SetNetwork(crate::network::NetworkSettings),
    /// Blocks a peer's address (`1.2.3.4:6881`) from now on. The engine restarts to apply it.
    BanPeer(String),
    /// Make a `.torrent` file from a file or folder, in the background. See [`crate::make`].
    CreateTorrent(crate::make::MakeOptions),
    /// Stop making the `.torrent` file.
    CancelCreate,
    /// Write a torrent's `.torrent` file here. Needs the torrent's details.
    SaveTorrentFile {
        key: String,
        path: PathBuf,
    },
    /// Notifications, keeping awake, theme, what to do when downloads finish.
    SetComfort(crate::comfort::ComfortSettings),
    /// Read one feed and show its items without downloading anything. `None` stops showing.
    PreviewFeed(Option<String>),
    /// Download one item of a feed by hand. The feed need not be saved yet.
    AddFeedItem {
        feed_url: String,
        item: feeds::Item,
        category: Option<String>,
    },
    /// A `.torrent` file's contents, sent from the remote-control page.
    AddTorrentBytes(Vec<u8>),
    /// Serve the remote-control page over HTTPS instead of plain HTTP.
    SetRemoteHttps(bool),
}

type Handle = Arc<ManagedTorrent>;
type OnChange = Arc<dyn Fn() + Send + Sync>;

const SAVE_EVERY: Duration = Duration::from_secs(30);
const KEEP_NOTICES: usize = 50;
const SCAN_EVERY: Duration = Duration::from_secs(5);
/// How often each feed is read, in seconds.
const FEED_EVERY: u64 = 15 * 60;
const FEED_MAX_BYTES: usize = 8 * 1024 * 1024;
const KEEP_SEEN: usize = 2000;
/// How long to wait before trying again to start the engine.
const RETRY_EVERY: Duration = Duration::from_secs(5);

struct Pending {
    id: u64,
    text: String,
    label: String,
    added_at: u64,
    /// The category the torrent gets once it is added.
    category: Option<String>,
    /// The work of adding it. `None` while there is no engine to add it to.
    task: Option<tokio::task::JoinHandle<()>>,
    /// Choices from the add dialog. See [`AddOptions`].
    folder: Option<PathBuf>,
    paused: bool,
    files: Option<Vec<usize>>,
}

struct FeedDone {
    url: String,
    /// The feed's text.
    result: Result<String, String>,
    /// Read only to be shown, not to download from.
    preview: bool,
}

struct AddDone {
    id: u64,
    text: String,
    label: String,
    /// The info-hash and name, and whether the torrent was already in the list.
    result: Result<(String, String, bool), String>,
}

struct Meta {
    files: Arc<Vec<FileMeta>>,
    trackers: Arc<Vec<String>>,
}

struct Worker {
    /// `None` while all traffic is blocked.
    session: Option<Arc<Session>>,
    data_dir: PathBuf,
    /// The interface the running session is tied to, and the number the system gave it.
    bound: Option<String>,
    bound_index: Option<u32>,
    /// Why there is no session.
    blocked: Option<String>,
    last_try: Option<Instant>,
    interfaces: Vec<Interface>,
    store: Store,
    log_path: PathBuf,
    shared: Shared,
    on_change: OnChange,
    notices: Vec<Notice>,
    pending: Vec<Pending>,
    next_pending: u64,
    add_tx: UnboundedSender<AddDone>,
    /// Upload counter last seen per torrent, to add only the difference to the lifetime total.
    last_up: HashMap<String, u64>,
    meta: HashMap<String, Meta>,
    watched: Option<String>,
    dirty: bool,
    last_save: Instant,
    last_scan: Option<Instant>,
    /// Files in the watch folder that could not be handled, so they are reported once.
    watch_failed: HashSet<PathBuf>,
    /// For reading feeds. Tied to the same interface as the session.
    http: Option<reqwest::Client>,
    feed_tx: UnboundedSender<FeedDone>,
    /// Feeds being read right now.
    feeds_busy: HashSet<String>,
    /// For the remote-control page, which sends commands the same way the window does.
    tx: UnboundedSender<Command>,
    remote_task: Option<tokio::task::JoinHandle<()>>,
    remote_error: Option<String>,
    remote_try: Option<Instant>,
    /// Name and file lists fetched for the add dialog.
    inspector: Inspector,
    /// Seeding time is counted in whole seconds up to here.
    seed_clock: Instant,
    /// Since when each running torrent has been slower than the slow-torrent speeds.
    slow_since: HashMap<String, Instant>,
    /// The speed limits last given to the engine, in KiB/s. `None` sets them again.
    applied_limits: Option<(u32, u32)>,
    /// The peers per torrent the running session was started with.
    session_peers: u32,
    /// The network settings the running session was started with, and what it reports.
    net_used: Option<(crate::network::NetworkSettings, crate::network::NetworkStatus)>,
    /// The `.torrent` file being made, or the last one made.
    making: Option<make::Making>,
    /// Counts the `.torrent` files made.
    jobs: u64,
    /// What each torrent's file says about itself, by key.
    details: Arc<HashMap<String, crate::make::Details>>,
    comfort: crate::comfort::Tracker,
    preview: Option<crate::model::FeedPreview>,
    remote_fingerprint: Option<String>,
}

mod make;

/// How long a torrent must stay below the slow-torrent speeds to count as slow.
const SLOW_AFTER: Duration = Duration::from_secs(60);

pub(crate) fn run(
    data_dir: PathBuf,
    rx: UnboundedReceiver<Command>,
    tx: UnboundedSender<Command>,
    shared: Shared,
    on_change: OnChange,
) {
    let fail = |text: String| {
        *shared.lock().unwrap_or_else(|e| e.into_inner()) = Arc::new(Snapshot {
            fatal: Some(text),
            ..Default::default()
        });
        on_change();
    };
    let runtime = match tokio::runtime::Builder::new_multi_thread().enable_all().build() {
        Ok(rt) => rt,
        Err(e) => return fail(format!("Could not start: {e}")),
    };
    if let Err(e) = runtime.block_on(main_loop(data_dir, rx, tx, shared.clone(), on_change.clone())) {
        fail(format!("{e:#}"));
    }
    runtime.shutdown_timeout(Duration::from_secs(2));
}

fn limit(kib: u32) -> Option<NonZeroU32> {
    NonZeroU32::new(kib.saturating_mul(1024))
}

async fn main_loop(
    data_dir: PathBuf,
    mut rx: UnboundedReceiver<Command>,
    tx: UnboundedSender<Command>,
    shared: Shared,
    on_change: OnChange,
) -> anyhow::Result<()> {
    std::fs::create_dir_all(&data_dir).map_err(|e| anyhow::anyhow!("Could not create {}: {e}", data_dir.display()))?;
    let (store, note) = Store::load(&data_dir.join("state.json"))?;

    let (add_tx, mut add_rx) = tokio::sync::mpsc::unbounded_channel();
    let (feed_tx, mut feed_rx) = tokio::sync::mpsc::unbounded_channel();
    let comfort = crate::comfort::Tracker::new(&store.data.all_time);
    let mut w = Worker {
        session: None,
        bound: None,
        bound_index: None,
        blocked: None,
        last_try: None,
        interfaces: Vec::new(),
        store,
        log_path: data_dir.join("rusttorrent.log"),
        data_dir,
        shared,
        on_change,
        notices: Vec::new(),
        pending: Vec::new(),
        next_pending: 0,
        add_tx,
        last_up: HashMap::new(),
        meta: HashMap::new(),
        watched: None,
        dirty: false,
        last_save: Instant::now(),
        last_scan: None,
        watch_failed: HashSet::new(),
        http: None,
        feed_tx,
        feeds_busy: HashSet::new(),
        tx,
        remote_task: None,
        remote_error: None,
        remote_try: None,
        inspector: Inspector::default(),
        seed_clock: Instant::now(),
        slow_since: HashMap::new(),
        applied_limits: None,
        session_peers: 0,
        net_used: None,
        making: None,
        jobs: 0,
        details: Arc::default(),
        comfort,
        preview: None,
        remote_fingerprint: None,
    };
    if let Some(note) = note {
        w.notice(note, true);
    }
    // Links whose details had not arrived when the app last quit.
    for text in std::mem::take(&mut w.store.data.pending) {
        w.add(text, None);
    }
    w.tick().await;

    let mut timer = tokio::time::interval(Duration::from_secs(1));
    timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            command = rx.recv() => match command {
                None | Some(Command::Shutdown) => break,
                Some(command) => {
                    w.handle(command).await;
                    // Take everything already waiting before doing the work of a tick.
                    let mut stop = false;
                    while let Ok(command) = rx.try_recv() {
                        if matches!(command, Command::Shutdown) {
                            stop = true;
                            break;
                        }
                        w.handle(command).await;
                    }
                    if stop {
                        break;
                    }
                    w.tick().await;
                }
            },
            Some(done) = add_rx.recv() => {
                w.added(done);
                w.tick().await;
            }
            Some(done) = feed_rx.recv() => {
                w.feed_done(done);
                w.tick().await;
            }
            _ = timer.tick() => w.tick().await,
        }
    }

    if let Some(task) = w.remote_task.take() {
        task.abort();
    }
    w.stop_session().await;
    w.save();
    Ok(())
}

impl Worker {
    fn notice(&mut self, text: impl Into<String>, error: bool) {
        let notice = Notice {
            at: now_secs(),
            text: text.into(),
            error,
        };
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.log_path)
        {
            let _ = writeln!(
                f,
                "{} {} {}",
                notice.at,
                if error { "ERROR" } else { "info" },
                notice.text
            );
        }
        self.notices.push(notice);
        if self.notices.len() > KEEP_NOTICES {
            let extra = self.notices.len() - KEEP_NOTICES;
            self.notices.drain(..extra);
        }
    }

    fn save(&mut self) {
        self.store.data.all_time.running_secs = self.comfort.running_secs();
        match self.store.save() {
            Ok(()) => {
                self.dirty = false;
                self.last_save = Instant::now();
            }
            Err(e) => self.notice(format!("Could not save settings: {e:#}"), true),
        }
    }

    fn handles(&self) -> Vec<Handle> {
        match &self.session {
            Some(session) => session.with_torrents(|it| it.map(|(_, h)| h.clone()).collect()),
            None => Vec::new(),
        }
    }

    fn find(&self, key: &str) -> Option<Handle> {
        self.handles().into_iter().find(|h| h.info_hash().as_string() == key)
    }

    fn add(&mut self, text: String, category: Option<String>) {
        self.add_with(AddOptions {
            text,
            category,
            ..Default::default()
        });
    }

    fn add_with(&mut self, options: AddOptions) {
        let AddOptions {
            text,
            folder,
            category,
            paused,
            files,
        } = options;
        let source = match intake::parse(&text) {
            Ok(s) => s,
            Err(e) => return self.notice(e, true),
        };
        let text = text.trim().to_owned();
        let prepared = self.inspector.take(&text);
        if self.pending.iter().any(|p| p.text == text) {
            return;
        }
        let label = intake::label(&source);
        let id = self.next_pending;
        self.next_pending += 1;
        let mut pending = Pending {
            id,
            text: text.clone(),
            label,
            added_at: now_secs(),
            category,
            task: None,
            folder,
            paused,
            files,
        };
        pending.task = self.start_add(&pending, prepared);
        self.pending.push(pending);
        self.store.data.pending.push(text);
        self.save();
    }

    /// Starts the work of adding a link or file to the engine, if there is an engine.
    fn start_add(&self, p: &Pending, prepared: Option<Prepared>) -> Option<tokio::task::JoinHandle<()>> {
        let session = self.session.clone()?;
        let source = intake::parse(&p.text).ok()?;
        let tx = self.add_tx.clone();
        let folder = manage::folder_for(&self.store.data.settings, p.folder.as_ref(), p.category.as_deref());
        let options = manage::engine_options(&folder, p.paused, p.files.clone());
        let (id, text, label) = (p.id, p.text.clone(), p.label.clone());
        Some(tokio::spawn(async move {
            let result = add_to_engine(&session, source, options, prepared).await;
            let _ = tx.send(AddDone {
                id,
                text,
                label,
                result,
            });
        }))
    }

    /// Stops the engine, so that nothing is sent or received until a new one starts.
    async fn stop_session(&mut self) {
        self.collect_uploads();
        for p in &mut self.pending {
            if let Some(task) = p.task.take() {
                task.abort();
            }
        }
        self.inspector.stop();
        if let Some(session) = self.session.take() {
            session.stop().await;
        }
        self.http = None;
        self.bound = None;
        self.bound_index = None;
        self.last_up.clear();
        self.slow_since.clear();
        self.applied_limits = None;
        self.net_used = None;
        self.comfort.forget_counters();
    }

    async fn start_session(&mut self, bind: Option<String>) -> anyhow::Result<()> {
        let settings = &self.store.data.settings;
        let network = settings.network.clone();
        let prepared = crate::network::prepare(&network, bind.as_deref(), &self.data_dir).await?;
        let options = SessionOptions {
            fastresume: true,
            persistence: Some(SessionPersistenceConfig::Json {
                folder: Some(self.data_dir.join("session")),
            }),
            listen: Some(prepared.listen),
            ratelimits: LimitsConfig {
                download_bps: limit(settings.down_limit_kib),
                upload_bps: limit(settings.up_limit_kib),
            },
            client_name_and_version: Some(format!("RustTorrent {}", crate::VERSION)),
            bind_device_name: bind.clone(),
            peer_limit: match settings.limits.peers_per_torrent {
                0 => None,
                n => Some(n as usize),
            },
            dht: prepared.dht,
            disable_local_service_discovery: prepared.disable_local_service_discovery,
            connect: prepared.connect,
            blocklist_url: prepared.blocklist_url,
            ..Default::default()
        };
        let peers = settings.limits.peers_per_torrent;
        // Read before the session starts: if the interface is replaced while it starts,
        // the next look sees a different number and starts again.
        let index = bind.as_deref().and_then(net::index);
        let session = Session::new_with_opts(settings.download_dir.clone(), options).await?;
        self.http = http_client(bind.as_deref(), network.proxy.url().as_deref());
        let status = crate::network::NetworkStatus {
            port: session.listen_addr().map(|a| a.port()),
            ..prepared.status
        };
        self.net_used = Some((network, status));
        self.session = Some(session);
        self.bound = bind;
        self.bound_index = index;
        self.session_peers = peers;
        for i in 0..self.pending.len() {
            if self.pending[i].task.is_none() {
                let task = self.start_add(&self.pending[i], None);
                self.pending[i].task = task;
            }
        }
        Ok(())
    }

    /// Makes the engine match the chosen interface: stopped while that interface is not
    /// connected or after the choice changed, running otherwise.
    async fn ensure_session(&mut self) {
        self.interfaces = net::interfaces();
        let wanted = self.store.data.settings.bind_interface.clone();
        let up = wanted
            .as_ref()
            .is_none_or(|name| self.interfaces.iter().any(|i| i.name == *name));
        if self.session.is_some() {
            let replaced = wanted.is_some() && wanted.as_deref().and_then(net::index) != self.bound_index;
            // The engine takes the number of peers per torrent only when it starts.
            let peers = self.session_peers == self.store.data.settings.limits.peers_per_torrent;
            let changed = self.net_used.as_ref().map(|(n, _)| n) != Some(&self.store.data.settings.network);
            if self.bound == wanted && up && !replaced && peers && !changed {
                return;
            }
            self.stop_session().await;
            self.blocked = None;
            self.last_try = None;
        }
        if !up {
            let name = wanted.unwrap_or_default();
            let text = format!("All traffic is blocked: the network interface {name} is not connected.");
            if self.blocked.as_deref() != Some(text.as_str()) {
                self.notice(text.clone(), true);
            }
            self.blocked = Some(text);
            self.last_try = None;
            return;
        }
        if self.last_try.is_some_and(|t| t.elapsed() < RETRY_EVERY) {
            return;
        }
        self.last_try = Some(Instant::now());
        match self.start_session(wanted.clone()).await {
            Ok(()) => {
                self.last_try = None;
                match (&wanted, self.blocked.take()) {
                    (Some(name), _) => self.notice(format!("All traffic goes through {name}."), false),
                    (None, Some(_)) => self.notice("The torrent engine is running again.", false),
                    (None, None) => {}
                }
            }
            Err(e) => {
                let text = format!("All traffic is blocked: the torrent engine could not start: {e:#}");
                let text = crate::network::redact(&text, &self.store.data.settings.network);
                if self.blocked.as_deref() != Some(text.as_str()) {
                    self.notice(text.clone(), true);
                }
                self.blocked = Some(text);
            }
        }
    }

    /// The list as it was last seen, for when there is no engine to ask.
    fn blocked_views(&self) -> Vec<TorrentView> {
        let settings = &self.store.data.settings;
        self.store
            .data
            .torrents
            .iter()
            .map(|(key, record)| TorrentView {
                key: key.clone(),
                name: if record.name.is_empty() {
                    key.clone()
                } else {
                    record.name.clone()
                },
                state: match record.intent {
                    Intent::Run => State::Blocked,
                    Intent::Pause if record.finished => State::Completed,
                    Intent::Pause => State::Paused,
                },
                error: None,
                total: record.total,
                done: record.done,
                uploaded: record.uploaded,
                down_bps: 0,
                up_bps: 0,
                peers_live: 0,
                peers_seen: 0,
                category: record.category.clone(),
                tags: record.tags.clone(),
                save_path: manage::folder_for(settings, None, record.category.as_deref())
                    .to_string_lossy()
                    .into_owned(),
                added_at: record.added_at,
                held: None,
                queue_pos: None,
                ratio_limit_pct: record.ratio_limit_pct,
                files: self.meta.get(key).map(|m| m.files.clone()).unwrap_or_default(),
                file_done: Vec::new(),
                selected: None,
                trackers: self.meta.get(key).map(|m| m.trackers.clone()).unwrap_or_default(),
                peers: Vec::new(),
                force_start: record.force_start,
                limits: TorrentLimits {
                    seeding: record.seeding,
                    ..Default::default()
                },
            })
            .collect()
    }

    fn added(&mut self, done: AddDone) {
        let (category, paused) = match self.pending.iter().position(|p| p.id == done.id) {
            Some(i) => {
                let p = self.pending.remove(i);
                (p.category, p.paused)
            }
            None => (None, false),
        };
        self.store.data.pending.retain(|t| *t != done.text);
        self.forget_upload(&done.text);
        match done.result {
            Ok((key, name, false)) => {
                if let Some(c) = &category {
                    self.store.data.settings.categories.insert(c.clone());
                }
                let queue_pos = self.next_queue_pos();
                self.store.data.torrents.insert(
                    key,
                    Record {
                        intent: if paused { Intent::Pause } else { Intent::Run },
                        added_at: now_secs(),
                        queue_pos,
                        category,
                        ..Default::default()
                    },
                );
                self.notice(format!("Added {name}"), false);
            }
            Ok((_, name, true)) => self.notice(format!("{name} is already in the list."), false),
            Err(e) => self.notice(format!("Could not add {}: {e}", done.label), true),
        }
        self.save();
    }

    /// The place after the last one in the queue.
    fn next_queue_pos(&self) -> u64 {
        self.store
            .data
            .torrents
            .values()
            .map(|r| r.queue_pos)
            .max()
            .unwrap_or(0)
            + 1
    }

    fn item(&self, key: &str, stats: &librqbit::TorrentStats) -> rules::Item {
        let record = self.store.data.torrents.get(key).cloned().unwrap_or_default();
        rules::Item {
            key: key.to_owned(),
            intent: record.intent,
            finished: stats.finished,
            settled: matches!(stats.state, TorrentStatsState::Live | TorrentStatsState::Paused),
            done: stats.progress_bytes,
            uploaded: record.uploaded,
            queue_pos: record.queue_pos,
            added_at: record.added_at,
            ratio_limit_pct: record.ratio_limit_pct,
            force_start: record.force_start,
            seeding: record.seeding,
            slow: self.slow_since.get(key).is_some_and(|t| t.elapsed() >= SLOW_AFTER),
        }
    }

    fn set_intent(&mut self, keys: &[String], intent: Intent) {
        for key in keys {
            if let Some(record) = self.store.data.torrents.get_mut(key) {
                record.intent = intent;
                // Resuming starts the time without uploads again.
                if intent == Intent::Run {
                    record.seeding.idle_secs = 0;
                }
            }
        }
        self.save();
    }

    /// Stops the remote-control server and, if it is switched on, starts it again with
    /// the settings as they are now. Everybody signed in has to sign in again.
    async fn start_remote(&mut self) {
        if let Some(task) = self.remote_task.take() {
            task.abort();
            // Let go of the port before asking for it again.
            let _ = task.await;
        }
        let before = self.remote_error.take();
        self.remote_try = Some(Instant::now());
        let remote = self.store.data.settings.remote.clone();
        self.remote_fingerprint = None;
        if !remote.enabled {
            return;
        }
        let tls = match remote.https.then(|| crate::remote::tls::acceptor(&self.data_dir)) {
            None => None,
            Some(Ok((acceptor, fingerprint))) => {
                self.remote_fingerprint = Some(fingerprint);
                Some(acceptor)
            }
            Some(Err(e)) => {
                self.remote_error = Some(format!("Remote control could not set up HTTPS: {e}"));
                return;
            }
        };
        let (Some(salt), Some(hash)) = (crate::remote::unhex(&remote.salt), crate::remote::unhex(&remote.hash)) else {
            self.remote_error = Some("The saved password cannot be read. Set a new one.".into());
            return;
        };
        match crate::remote::listen(remote.lan, remote.port).await {
            Ok(listener) => {
                let who = if remote.lan {
                    "this network"
                } else {
                    "this computer only"
                };
                self.notice(format!("Remote control is on at port {} ({who}).", remote.port), false);
                self.remote_task = Some(tokio::spawn(crate::remote::serve(
                    listener,
                    salt,
                    hash,
                    self.shared.clone(),
                    self.tx.clone(),
                    tls,
                )));
            }
            Err(e) => {
                let text = format!("Remote control could not use port {}: {e}", remote.port);
                // Tried again every few seconds; said once.
                if before.as_deref() != Some(&text) {
                    self.notice(text.clone(), true);
                }
                self.remote_error = Some(text);
            }
        }
    }

    async fn handle(&mut self, command: Command) {
        match command {
            Command::Add(text) => self.add(text, None),
            Command::Pause(keys) => self.set_intent(&keys, Intent::Pause),
            Command::Resume(keys) => self.set_intent(&keys, Intent::Run),
            Command::Remove { keys, delete_files } => {
                for key in keys {
                    if let Some(id) = key.strip_prefix("pending:").and_then(|n| n.parse::<u64>().ok()) {
                        if let Some(i) = self.pending.iter().position(|p| p.id == id) {
                            let p = self.pending.remove(i);
                            if let Some(task) = p.task {
                                task.abort();
                            }
                            self.store.data.pending.retain(|t| *t != p.text);
                            self.forget_upload(&p.text);
                        }
                        continue;
                    }
                    let Some(session) = self.session.clone() else {
                        self.notice("Torrents cannot be removed while all traffic is blocked.", true);
                        break;
                    };
                    let Some(handle) = self.find(&key) else { continue };
                    let name = handle.name().unwrap_or_else(|| key.clone());
                    match session.delete(TorrentIdOrHash::Id(handle.id()), delete_files).await {
                        Ok(()) => {
                            self.store.data.torrents.remove(&key);
                            self.last_up.remove(&key);
                            self.slow_since.remove(&key);
                            self.meta.remove(&key);
                            let what = if delete_files {
                                "Removed and deleted files of"
                            } else {
                                "Removed"
                            };
                            self.notice(format!("{what} {name}"), false);
                        }
                        Err(e) => self.notice(format!("Could not remove {name}: {e:#}"), true),
                    }
                }
                self.save();
            }
            Command::SetFiles { key, files } => {
                let Some(session) = self.session.clone() else {
                    return self.notice("Files cannot be chosen while all traffic is blocked.", true);
                };
                let Some(handle) = self.find(&key) else { return };
                let set: HashSet<usize> = files.into_iter().collect();
                if let Err(e) = session.update_only_files(&handle, &set).await {
                    let name = handle.name().unwrap_or(key);
                    self.notice(format!("Could not change the chosen files of {name}: {e:#}"), true);
                }
            }
            Command::SetLimits { down_kib, up_kib } => {
                self.store.data.settings.down_limit_kib = down_kib;
                self.store.data.settings.up_limit_kib = up_kib;
                if let Some(session) = &self.session {
                    session.ratelimits.set_download_bps(limit(down_kib));
                    session.ratelimits.set_upload_bps(limit(up_kib));
                }
                // Slow mode may be on; the next look sets the limits that apply.
                self.applied_limits = None;
                self.save();
            }
            Command::SetDownloadDir(dir) => {
                self.store.data.settings.download_dir = dir;
                self.save();
            }
            Command::SetRules {
                max_downloads,
                ratio_limit_pct,
                schedule,
            } => {
                let settings = &mut self.store.data.settings;
                settings.max_downloads = max_downloads;
                settings.ratio_limit_pct = ratio_limit_pct;
                settings.schedule = schedule;
                self.save();
            }
            Command::SetRatioLimit { keys, limit } => {
                for key in keys {
                    if let Some(record) = self.store.data.torrents.get_mut(&key) {
                        record.ratio_limit_pct = limit;
                    }
                }
                self.save();
            }
            Command::MoveInQueue { keys, how } => {
                let items: Vec<rules::Item> = self
                    .handles()
                    .iter()
                    .map(|h| self.item(&h.info_hash().as_string(), &h.stats()))
                    .collect();
                for (i, key) in rules::reorder(&items, &keys, how).into_iter().enumerate() {
                    if let Some(record) = self.store.data.torrents.get_mut(&key) {
                        record.queue_pos = i as u64 + 1;
                    }
                }
                self.save();
            }
            Command::SetCategory { keys, category } => {
                let category = category.map(|c| c.trim().to_owned()).filter(|c| !c.is_empty());
                if let Some(name) = &category {
                    self.store.data.settings.categories.insert(name.clone());
                }
                for key in keys {
                    if let Some(record) = self.store.data.torrents.get_mut(&key) {
                        record.category = category.clone();
                    }
                }
                self.save();
            }
            Command::SetTag { keys, tag, on } => {
                let tag = tag.trim().to_owned();
                if tag.is_empty() {
                    return;
                }
                for key in keys {
                    if let Some(record) = self.store.data.torrents.get_mut(&key) {
                        record.tags.retain(|t| *t != tag);
                        if on {
                            record.tags.push(tag.clone());
                            record.tags.sort();
                        }
                    }
                }
                self.save();
            }
            Command::SetInterface(name) => {
                self.store.data.settings.bind_interface = name.map(|n| n.trim().to_owned()).filter(|n| !n.is_empty());
                self.save();
            }
            Command::SetRemote {
                enabled,
                port,
                lan,
                password,
            } => {
                if let Some(password) = password {
                    if password.chars().count() < crate::remote::MIN_PASSWORD {
                        self.notice(
                            format!(
                                "The remote-control password needs at least {} characters. It was not changed.",
                                crate::remote::MIN_PASSWORD
                            ),
                            true,
                        );
                    } else if let Ok((salt, hash)) =
                        tokio::task::spawn_blocking(move || crate::remote::hash_password(&password)).await
                    {
                        let remote = &mut self.store.data.settings.remote;
                        (remote.salt, remote.hash) = (salt, hash);
                    }
                }
                let has_password = self.store.data.settings.remote.has_password();
                if enabled && !has_password {
                    self.notice("Remote control stays off until it has a password.", true);
                }
                let remote = &mut self.store.data.settings.remote;
                remote.enabled = enabled && has_password;
                remote.port = if port == 0 { 8787 } else { port };
                remote.lan = lan;
                self.save();
                self.start_remote().await;
            }
            Command::SetWatchDir(dir) => {
                self.store.data.settings.watch_dir = dir;
                self.watch_failed.clear();
                self.last_scan = None;
                self.save();
            }
            Command::SetFeeds(feeds) => {
                let mut kept: Vec<Feed> = Vec::new();
                for mut feed in feeds {
                    feed.url = feed.url.trim().to_owned();
                    feed.name = feed.name.trim().to_owned();
                    feed.category = feed.category.map(|c| c.trim().to_owned()).filter(|c| !c.is_empty());
                    if !feed.url.is_empty() && !kept.iter().any(|f| f.url == feed.url) {
                        kept.push(feed);
                    }
                }
                let data = &mut self.store.data;
                data.feed_state.retain(|url, _| kept.iter().any(|f| f.url == *url));
                // A changed filter should be tried at once, not in a quarter of an hour.
                for feed in &kept {
                    if !data.feeds.contains(feed) {
                        if let Some(state) = data.feed_state.get_mut(&feed.url) {
                            state.checked_at = 0;
                            // Turned on just now: what the feed lists now counts as old.
                            if feed.newer_only && !data.feeds.iter().any(|f| f.url == feed.url && f.newer_only) {
                                state.primed = false;
                            }
                        }
                    }
                }
                data.feeds = kept;
                self.save();
            }
            Command::CheckFeeds => {
                for state in self.store.data.feed_state.values_mut() {
                    state.checked_at = 0;
                }
            }
            Command::Watch(key) => self.watched = key,
            Command::Inspect(texts) => self.inspector.want(texts, self.session.as_ref()),
            Command::AddWith(options) => self.add_with(options),
            Command::SetForceStart { keys, on } => {
                for key in &keys {
                    if let Some(record) = self.store.data.torrents.get_mut(key) {
                        record.force_start = on;
                    }
                }
                self.save();
            }
            Command::SetCategoryDir { category, dir } => {
                let dirs = &mut self.store.data.settings.category_dirs;
                match dir {
                    Some(dir) => dirs.insert(category, dir),
                    None => dirs.remove(&category),
                };
                self.save();
            }
            Command::AddCategory(name) => {
                let name = name.trim();
                if !name.is_empty() {
                    self.store.data.settings.categories.insert(name.to_owned());
                    self.save();
                }
            }
            Command::RemoveCategory(name) => {
                let data = &mut self.store.data;
                data.settings.categories.remove(&name);
                data.settings.category_dirs.remove(&name);
                for record in data.torrents.values_mut() {
                    if record.category.as_ref() == Some(&name) {
                        record.category = None;
                    }
                }
                self.save();
            }
            Command::SetAskBeforeAdd(on) => {
                self.store.data.settings.ask_before_add = on;
                self.save();
            }
            Command::CreateTorrent(opts) => self.start_make(opts),
            Command::CancelCreate => self.cancel_make(),
            Command::SaveTorrentFile { key, path } => self.save_torrent_file(&key, path),
            Command::Shutdown => {}
            Command::SetSlowMode(on) => {
                rules::set_slow_mode(&mut self.store.data.settings.limits.slow, on, local_clock());
                self.save();
            }
            Command::SetLimitSettings(mut limits) => {
                let settings = &mut self.store.data.settings;
                // The switch is changed with its own command only.
                limits.slow.on = settings.limits.slow.on;
                limits.slow.skipped =
                    settings.limits.slow.skipped && limits.slow.schedule == settings.limits.slow.schedule;
                settings.limits = limits;
                self.save();
            }
            Command::SetSeedingLimits {
                keys,
                time_limit_min,
                idle_limit_min,
            } => {
                for key in keys {
                    if let Some(record) = self.store.data.torrents.get_mut(&key) {
                        record.seeding.time_limit_min = time_limit_min;
                        record.seeding.idle_limit_min = idle_limit_min;
                    }
                }
                self.save();
            }
            Command::SetNetwork(network) => {
                self.store.data.settings.network = crate::network::clean(network);
                // Try at once with the new settings, even if the last start failed moments ago.
                self.last_try = None;
                self.save();
            }
            Command::BanPeer(addr) => match crate::network::ban(&mut self.store.data.settings.network, &addr) {
                Ok(ip) => {
                    self.notice(
                        format!(
                            "Banned {ip}. The engine restarts to apply this, so transfers pause for a few seconds."
                        ),
                        false,
                    );
                    self.last_try = None;
                    self.save();
                }
                Err(text) => self.notice(text, true),
            },
            Command::SetComfort(comfort) => {
                let settings = &mut self.store.data.settings;
                if comfort.when_all_done != settings.comfort.when_all_done {
                    self.comfort.disarm();
                }
                settings.comfort = comfort;
                self.save();
            }
            Command::PreviewFeed(url) => self.preview_feed(url),
            Command::AddFeedItem {
                feed_url,
                item,
                category,
            } => self.add_feed_item(feed_url, item, category),
            Command::AddTorrentBytes(bytes) => self.add_upload(bytes),
            Command::SetRemoteHttps(on) => {
                self.store.data.settings.remote.https = on;
                self.save();
                self.start_remote().await;
            }
        }
    }

    /// A download finished just now: say so and run the user's program, if any.
    fn finished(&mut self, key: &str, handle: &Handle) {
        let name = handle.name().unwrap_or_else(|| key.to_owned());
        let folder = handle.output_folder().to_path_buf();
        self.notice(format!("Finished downloading {name}"), false);
        let program = self.store.data.settings.comfort.on_finish_program.clone();
        if !program.trim().is_empty() {
            if let Err(e) = crate::comfort::run_finish_program(&program, &name, &folder) {
                self.notice(e, true);
            }
        }
        (self.comfort).add_finished(key.to_owned(), name, folder.to_string_lossy().into_owned());
    }

    /// Adds the `.torrent` files found in the watch folder. Each one is renamed first, so
    /// it is picked up once only and the folder shows what has been taken.
    fn scan_watch_dir(&mut self) {
        let Some(dir) = self.store.data.settings.watch_dir.clone() else {
            return;
        };
        let Ok(entries) = std::fs::read_dir(&dir) else { return };
        let mut found: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.extension().is_some_and(|x| x.eq_ignore_ascii_case("torrent"))
                    && !self.watch_failed.contains(p)
                    // Leave a file alone while it may still be being written.
                    && std::fs::metadata(p).is_ok_and(|m| {
                        m.is_file() && m.modified().ok().and_then(|t| t.elapsed().ok()).is_none_or(|age| age >= Duration::from_secs(2))
                    })
            })
            .collect();
        found.sort();
        for path in found {
            let mut taken = path.clone().into_os_string();
            taken.push(".added");
            let taken = PathBuf::from(taken);
            match std::fs::rename(&path, &taken) {
                Ok(()) => self.add(taken.to_string_lossy().into_owned(), None),
                Err(e) => {
                    self.notice(
                        format!("Could not take {} from the watch folder: {e}", path.display()),
                        true,
                    );
                    self.watch_failed.insert(path);
                }
            }
        }
    }

    /// Starts reading every feed whose turn has come.
    fn poll_feeds(&mut self) {
        let Some(http) = self.http.clone() else { return };
        let now = now_secs();
        for feed in &self.store.data.feeds {
            let checked_at = self.store.data.feed_state.get(&feed.url).map_or(0, |s| s.checked_at);
            if !feed.enabled || now < checked_at.saturating_add(FEED_EVERY) || self.feeds_busy.contains(&feed.url) {
                continue;
            }
            self.feeds_busy.insert(feed.url.clone());
            let (http, tx, url) = (http.clone(), self.feed_tx.clone(), feed.url.clone());
            tokio::spawn(async move {
                let result = fetch_feed(&http, &url).await;
                let _ = tx.send(FeedDone {
                    url,
                    result,
                    preview: false,
                });
            });
        }
    }

    fn feed_done(&mut self, done: FeedDone) {
        if done.preview {
            return self.preview_done(done);
        }
        self.feeds_busy.remove(&done.url);
        let Some(feed) = self.store.data.feeds.iter().find(|f| f.url == done.url).cloned() else {
            return;
        };
        let shown = if feed.name.is_empty() {
            feed.url.clone()
        } else {
            feed.name.clone()
        };
        let mut state = self.store.data.feed_state.remove(&feed.url).unwrap_or_default();
        state.checked_at = now_secs();
        let result = done.result.and_then(|text| {
            let items = feeds::parse(&text);
            if items.is_empty() {
                Err("no items with a link were found in it".to_owned())
            } else {
                Ok(items)
            }
        });
        // A filter saved by hand in the settings file may not be valid.
        let result = result.and_then(|items| feed.rule().map(|rule| (rule, items)));
        match result {
            Ok((rule, items)) => {
                state.error = None;
                if rule.first_read(state.primed) {
                    self.notice(
                        format!(
                            "Feed {shown}: the {} items it lists now are skipped. Newer ones will be added.",
                            items.len()
                        ),
                        false,
                    );
                }
                // Oldest first, so torrents join the queue in the order they were published.
                let picked = rule.pick(items, state.primed, &mut state.seen, &mut state.episodes);
                state.primed = true;
                for item in picked {
                    if !feed_preview::is_link(&item.link) {
                        self.notice(format!("Feed {shown}: {} has no web or magnet link.", item.title), true);
                        continue;
                    }
                    self.notice(format!("Feed {shown}: adding {}", item.title), false);
                    self.add(item.link, feed.category.clone());
                }
                for list in [&mut state.seen, &mut state.episodes] {
                    if list.len() > KEEP_SEEN {
                        let extra = list.len() - KEEP_SEEN;
                        list.drain(..extra);
                    }
                }
            }
            Err(e) => {
                // Say it once, not every quarter of an hour.
                if state.error.as_deref() != Some(e.as_str()) {
                    self.notice(format!("Could not read feed {shown}: {e}"), true);
                }
                state.error = Some(e);
            }
        }
        self.store.data.feed_state.insert(feed.url, state);
        self.save();
    }

    /// Adds what each running torrent uploaded since the last look to its lifetime total.
    /// The engine's counter starts again from zero whenever a torrent restarts.
    fn collect_uploads(&mut self) {
        for handle in self.handles() {
            let key = handle.info_hash().as_string();
            let stats = handle.stats();
            self.count_upload(
                &key,
                matches!(stats.state, TorrentStatsState::Live),
                stats.uploaded_bytes,
            );
        }
    }

    fn count_upload(&mut self, key: &str, live: bool, current: u64) {
        if !live {
            self.last_up.remove(key);
            return;
        }
        let last = self.last_up.insert(key.to_owned(), current).unwrap_or(0);
        let delta = if current >= last { current - last } else { current };
        if delta > 0 {
            if let Some(record) = self.store.data.torrents.get_mut(key) {
                record.uploaded += delta;
                self.dirty = true;
            }
            self.store.data.all_time.uploaded += delta;
        }
    }

    fn meta_for(&mut self, key: &str, handle: &Handle) -> (Arc<Vec<FileMeta>>, Arc<Vec<String>>) {
        if !self.meta.contains_key(key) {
            let files = handle
                .with_metadata(|m| {
                    m.file_infos
                        .iter()
                        .map(|f| FileMeta {
                            name: f.relative_filename.to_string_lossy().into_owned(),
                            len: f.len,
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            let mut trackers: Vec<String> = handle.shared().trackers.iter().map(|u| u.to_string()).collect();
            trackers.sort();
            // Details may not be there yet; only remember them once they are.
            if files.is_empty() {
                return (Arc::new(files), Arc::new(trackers));
            }
            self.meta.insert(
                key.to_owned(),
                Meta {
                    files: Arc::new(files),
                    trackers: Arc::new(trackers),
                },
            );
        }
        let m = &self.meta[key];
        (m.files.clone(), m.trackers.clone())
    }

    async fn tick(&mut self) {
        if self.last_scan.is_none_or(|t| t.elapsed() >= SCAN_EVERY) {
            self.last_scan = Some(Instant::now());
            self.scan_watch_dir();
        }
        self.ensure_session().await;
        self.poll_feeds();

        let mut torrents = Vec::new();
        let (mut down_total, mut up_total) = (0, 0);
        let clock = local_clock();
        let mut pieces = None;
        let schedule_off = !self.store.data.settings.schedule.allows(clock);
        if rules::slow_mode_tick(&mut self.store.data.settings.limits.slow, clock) {
            self.dirty = true;
        }
        let (down_kib, up_kib, slow) = rules::speed_limits(&self.store.data.settings, clock);
        if let Some(session) = &self.session {
            if self.applied_limits != Some((down_kib, up_kib)) {
                session.ratelimits.set_download_bps(limit(down_kib));
                session.ratelimits.set_upload_bps(limit(up_kib));
                self.applied_limits = Some((down_kib, up_kib));
            }
        }
        let seed_secs = self.seed_clock.elapsed().as_secs();
        self.seed_clock += Duration::from_secs(seed_secs);

        if let Some(session) = self.session.clone() {
            // First look at every torrent, then let the rules see them all together.
            let mut rows = Vec::new();
            for handle in self.handles() {
                let key = handle.info_hash().as_string();
                let stats = handle.stats();
                if !self.store.data.torrents.contains_key(&key) {
                    let queue_pos = self.next_queue_pos();
                    // The engine knows a torrent the store does not (for example the state file
                    // was lost). Keep it as it is rather than starting or stopping it.
                    let intent = if handle.is_paused() { Intent::Pause } else { Intent::Run };
                    self.store.data.torrents.insert(
                        key.clone(),
                        Record {
                            intent,
                            added_at: now_secs(),
                            queue_pos,
                            ..Default::default()
                        },
                    );
                    self.dirty = true;
                }
                let live = matches!(stats.state, TorrentStatsState::Live);
                let before = self.store.data.torrents.get(&key).map_or(0, |r| r.uploaded);
                self.count_upload(&key, live, stats.uploaded_bytes);
                self.watch_limits(&key, &stats, live, seed_secs, before);
                // Remember what the list shows, for when the engine is not running.
                if let Some(record) = self.store.data.torrents.get_mut(&key) {
                    let name = handle.name().unwrap_or_default();
                    let seen = (stats.total_bytes, stats.progress_bytes, stats.finished);
                    if (!name.is_empty() && record.name != name) || (record.total, record.done, record.finished) != seen
                    {
                        if !name.is_empty() {
                            record.name = name;
                        }
                        (record.total, record.done, record.finished) = seen;
                        self.dirty = true;
                    }
                }
                let downloaded = (stats.live.as_ref()).map(|l| l.snapshot.downloaded_and_checked_bytes);
                let delta = self.comfort.count_download(&key, downloaded);
                if delta > 0 {
                    self.store.data.all_time.downloaded += delta;
                    self.dirty = true;
                }
                if self.comfort.check_finish(&key, live, stats.finished) {
                    self.finished(&key, &handle);
                }
                rows.push((handle, key, stats));
            }
            let items: Vec<rules::Item> = rows.iter().map(|(_, key, stats)| self.item(key, stats)).collect();
            let outcomes = rules::apply(&items, &self.store.data.settings, clock);

            for ((handle, key, stats), outcome) in rows.into_iter().zip(outcomes) {
                // Make the engine match what the arbiter decides.
                let intent = self.store.data.torrents[&key].intent;
                let decision = decide(intent, outcome.holds);
                let should_run = decision == Decision::Run;
                let held = match decision {
                    Decision::Held(reason) => Some(reason),
                    _ => None,
                };
                let fix = match stats.state {
                    TorrentStatsState::Live if !should_run => Some((session.pause(&handle).await, Intent::Run)),
                    TorrentStatsState::Paused if should_run => Some((session.unpause(&handle).await, Intent::Pause)),
                    _ => None,
                };
                if let Some((Err(e), actual)) = fix {
                    let name = handle.name().unwrap_or_else(|| key.clone());
                    let verb = if should_run { "resume" } else { "pause" };
                    self.notice(format!("Could not {verb} {name}: {e:#}"), true);
                    // Record what is true, so this is not retried and reported every second.
                    if let Some(record) = self.store.data.torrents.get_mut(&key) {
                        record.intent = actual;
                    }
                    self.dirty = true;
                }

                let state = match stats.state {
                    TorrentStatsState::Initializing { .. } => State::Checking,
                    TorrentStatsState::Live if stats.finished => State::Seeding,
                    TorrentStatsState::Live => State::Downloading,
                    TorrentStatsState::Paused if held == Some(HoldReason::Schedule) => State::Scheduled,
                    TorrentStatsState::Paused if held == Some(HoldReason::Queue) => State::Queued,
                    TorrentStatsState::Paused if stats.finished => State::Completed,
                    TorrentStatsState::Paused => State::Paused,
                    TorrentStatsState::Error => State::Error,
                };
                let (down_bps, up_bps, peers_live, peers_seen) = match &stats.live {
                    Some(l) => (
                        l.download_speed.as_bytes(),
                        l.upload_speed.as_bytes(),
                        l.snapshot.peer_stats.live,
                        l.snapshot.peer_stats.seen,
                    ),
                    None => (0, 0, 0, 0),
                };
                down_total += down_bps;
                up_total += up_bps;

                let peers = if self.watched.as_deref() == Some(key.as_str()) {
                    pieces = crate::comfort::piece_map(&session, &handle).map(Arc::new);
                    peers_of(&handle)
                } else {
                    Vec::new()
                };
                let (files, trackers) = self.meta_for(&key, &handle);
                let record = &self.store.data.torrents[&key];
                torrents.push(TorrentView {
                    name: handle.name().unwrap_or_else(|| key.clone()),
                    state,
                    error: stats.error.clone(),
                    total: stats.total_bytes,
                    done: stats.progress_bytes,
                    uploaded: record.uploaded,
                    down_bps,
                    up_bps,
                    peers_live,
                    peers_seen,
                    category: record.category.clone(),
                    tags: record.tags.clone(),
                    save_path: handle.output_folder().to_string_lossy().into_owned(),
                    added_at: record.added_at,
                    held,
                    queue_pos: outcome.queue_pos,
                    ratio_limit_pct: record.ratio_limit_pct,
                    files,
                    file_done: stats.file_progress.clone(),
                    selected: handle.only_files(),
                    trackers,
                    peers,
                    force_start: record.force_start,
                    limits: TorrentLimits {
                        seeding: record.seeding,
                        stop: outcome.seed_stop,
                        slow: self.slow_since.get(&key).is_some_and(|t| t.elapsed() >= SLOW_AFTER),
                    },
                    key,
                });
            }
        } else {
            torrents = self.blocked_views();
        }

        let blocked = self.session.is_none();
        for p in &self.pending {
            torrents.push(TorrentView {
                key: format!("pending:{}", p.id),
                name: p.label.clone(),
                state: match (blocked, p.paused) {
                    (true, true) => State::Paused,
                    (true, false) => State::Blocked,
                    (false, _) => State::FetchingInfo,
                },
                error: None,
                total: 0,
                done: 0,
                uploaded: 0,
                down_bps: 0,
                up_bps: 0,
                peers_live: 0,
                peers_seen: 0,
                category: p.category.clone(),
                tags: Vec::new(),
                save_path: manage::folder_for(&self.store.data.settings, p.folder.as_ref(), p.category.as_deref())
                    .to_string_lossy()
                    .into_owned(),
                added_at: p.added_at,
                held: None,
                queue_pos: None,
                ratio_limit_pct: None,
                files: Arc::default(),
                file_done: Vec::new(),
                selected: p.files.clone(),
                trackers: Arc::default(),
                peers: Vec::new(),
                force_start: false,
                limits: TorrentLimits::default(),
            });
        }

        if self.dirty && self.last_save.elapsed() >= SAVE_EVERY {
            self.save();
        }

        // A port that was taken may be free by now.
        if self.store.data.settings.remote.enabled
            && self.remote_task.is_none()
            && self.remote_try.is_none_or(|at| at.elapsed() >= Duration::from_secs(15))
        {
            self.start_remote().await;
        }

        let (making, details) = self.make_tick().await;
        self.comfort.speeds.push(now_secs(), down_total, up_total);
        let quit_now = (self.comfort).should_quit(self.store.data.settings.comfort.when_all_done, &torrents);
        let snapshot = Snapshot {
            ready: true,
            fatal: None,
            torrents,
            down_bps: down_total,
            up_bps: up_total,
            settings: self.store.data.settings.clone(),
            schedule_off,
            feeds: self
                .store
                .data
                .feeds
                .iter()
                .map(|feed| {
                    let state = self.store.data.feed_state.get(&feed.url);
                    FeedView {
                        feed: feed.clone(),
                        checked_at: state.map_or(0, |s| s.checked_at),
                        error: state.and_then(|s| s.error.clone()),
                    }
                })
                .collect(),
            notices: self.notices.clone(),
            blocked: blocked.then(|| self.blocked.clone().unwrap_or_default()),
            interfaces: self.interfaces.clone(),
            remote_on: self.remote_task.is_some(),
            remote_error: self.remote_error.clone(),
            inspected: self.inspector.views(),
            slow,
            network: self.net_used.as_ref().map(|(_, s)| s.clone()).unwrap_or_default(),
            making,
            details,
            speeds: Arc::new(self.comfort.speeds.clone()),
            all_time: crate::comfort::AllTime {
                running_secs: self.comfort.running().0,
                ..self.store.data.all_time
            },
            running_since: self.comfort.running().1,
            finished: self.comfort.finished.clone(),
            quit_now,
            pieces,
            feed_preview: self.preview.clone(),
            remote_fingerprint: self.remote_fingerprint.clone(),
        };
        let mut current = self.shared.lock().unwrap_or_else(|e| e.into_inner());
        if **current != snapshot {
            *current = Arc::new(snapshot);
            drop(current);
            (self.on_change)();
        }
    }
}

impl Worker {
    /// Counts seeding time and notes which torrents are slow.
    fn watch_limits(&mut self, key: &str, stats: &librqbit::TorrentStats, live: bool, secs: u64, before: u64) {
        let limits = self.store.data.settings.limits;
        let slow = stats.live.as_ref().is_some_and(|l| {
            l.download_speed.as_bytes() < limits.slow_down_kib as u64 * 1024
                && l.upload_speed.as_bytes() < limits.slow_up_kib as u64 * 1024
        });
        if live && slow {
            self.slow_since.entry(key.to_owned()).or_insert_with(Instant::now);
        } else {
            self.slow_since.remove(key);
        }
        if !live || !stats.finished {
            return;
        }
        if let Some(record) = self.store.data.torrents.get_mut(key) {
            let uploaded = record.uploaded > before;
            rules::seed_tick(&mut record.seeding, secs, uploaded);
            if secs > 0 {
                self.dirty = true;
            }
        }
    }
}

/// Local time, as the schedules need it.
fn local_clock() -> Clock {
    let now = chrono::Local::now();
    Clock {
        weekday: now.weekday().num_days_from_monday() as u8,
        minute: (now.hour() * 60 + now.minute()) as u16,
    }
}

fn peers_of(handle: &Handle) -> Vec<PeerView> {
    let Some(live) = handle.live() else { return Vec::new() };
    let mut peers: Vec<PeerView> = live
        .per_peer_stats_snapshot(Default::default())
        .peers
        .into_iter()
        .map(|(addr, p)| PeerView {
            addr,
            client: p.client_name.unwrap_or_default(),
            connection: match p.conn_kind.map(|k| format!("{k:?}")).as_deref() {
                Some("Tcp") => "TCP",
                Some("Utp") => "uTP",
                Some("Socks") => "SOCKS",
                _ => "",
            },
            downloaded: p.counters.fetched_bytes,
            uploaded: p.counters.uploaded_bytes,
        })
        .collect();
    peers.sort_by(|a, b| b.downloaded.cmp(&a.downloaded).then_with(|| a.addr.cmp(&b.addr)));
    peers
}

/// The client that reads feeds, tied to the interface if one is chosen. `None` if it
/// cannot be tied to it: then feeds are not read at all rather than read some other way.
/// Feeds go through the proxy when one is set.
fn http_client(bind: Option<&str>, proxy: Option<&str>) -> Option<reqwest::Client> {
    let builder = reqwest::Client::builder()
        .user_agent(format!("RustTorrent/{}", crate::VERSION))
        .timeout(Duration::from_secs(30));
    let builder = match proxy {
        Some(url) => builder.proxy(reqwest::Proxy::all(url).ok()?),
        None => builder,
    };
    #[cfg(any(target_os = "macos", target_os = "linux", target_os = "android"))]
    let builder = match bind {
        Some(name) => builder.interface(name),
        None => builder,
    };
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "android")))]
    if bind.is_some() {
        return None;
    }
    builder.build().ok()
}

async fn fetch_feed(http: &reqwest::Client, url: &str) -> Result<String, String> {
    let plain = |e: reqwest::Error| format!("{:#}", anyhow::Error::from(e.without_url()));
    let response = http.get(url).send().await.map_err(plain)?;
    if !response.status().is_success() {
        return Err(format!("the server answered {}", response.status()));
    }
    let bytes = response.bytes().await.map_err(plain)?;
    if bytes.len() > FEED_MAX_BYTES {
        return Err("it is too large to be a feed".into());
    }
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

async fn add_to_engine(
    session: &Arc<Session>,
    source: Source,
    mut options: AddTorrentOptions,
    prepared: Option<Prepared>,
) -> Result<(String, String, bool), String> {
    let add = match (prepared, source) {
        (Some(prepared), _) => prepared.into_add(&mut options),
        (None, Source::Link(url)) => AddTorrent::from_url(url),
        (None, Source::File(path)) => {
            let bytes = tokio::task::spawn_blocking(move || {
                std::fs::read(&path).map_err(|e| format!("could not read {}: {e}", path.display()))
            })
            .await
            .map_err(|e| e.to_string())??;
            AddTorrent::from_bytes(bytes)
        }
    };
    let (handle, already) = match session
        .add_torrent(add, Some(options))
        .await
        .map_err(|e| format!("{e:#}"))?
    {
        AddTorrentResponse::Added(_, h) => (h, false),
        AddTorrentResponse::AlreadyManaged(_, h) => (h, true),
        AddTorrentResponse::ListOnly(_) => return Err("the engine only listed the torrent".into()),
    };
    let key = handle.info_hash().as_string();
    let name = handle.name().unwrap_or_else(|| key.clone());
    Ok((key, name, already))
}
