//! The only file that talks to librqbit.
//!
//! One worker owns the engine session and the store. It takes commands, and once a second
//! it compares what each torrent is doing with what the arbiter says it should be doing,
//! fixes any difference, and publishes a snapshot if anything changed.
use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::num::NonZeroU32;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use librqbit::api::TorrentIdOrHash;
use librqbit::limits::LimitsConfig;
use librqbit::{
    AddTorrent, AddTorrentOptions, AddTorrentResponse, ListenerOptions, ManagedTorrent, Session, SessionOptions,
    SessionPersistenceConfig, TorrentStatsState,
};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

use chrono::{Datelike, Timelike};

use crate::arbiter::{decide, Decision, HoldReason, Intent};
use crate::intake::{self, Source};
use crate::model::{FileMeta, Notice, PeerView, Snapshot, State, TorrentView};
use crate::rules::{self, Clock, Move, Schedule};
use crate::store::{Record, Store};
use crate::{now_secs, Shared};

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
    /// The torrent whose peers the front end wants to see.
    Watch(Option<String>),
    Shutdown,
}

type Handle = Arc<ManagedTorrent>;
type OnChange = Arc<dyn Fn() + Send + Sync>;

const SAVE_EVERY: Duration = Duration::from_secs(30);
const KEEP_NOTICES: usize = 50;

struct Pending {
    id: u64,
    text: String,
    label: String,
    added_at: u64,
    task: tokio::task::JoinHandle<()>,
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
    session: Arc<Session>,
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
}

pub(crate) fn run(data_dir: PathBuf, rx: UnboundedReceiver<Command>, shared: Shared, on_change: OnChange) {
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
    if let Err(e) = runtime.block_on(main_loop(data_dir, rx, shared.clone(), on_change.clone())) {
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
    shared: Shared,
    on_change: OnChange,
) -> anyhow::Result<()> {
    std::fs::create_dir_all(&data_dir).map_err(|e| anyhow::anyhow!("Could not create {}: {e}", data_dir.display()))?;
    let (store, note) = Store::load(&data_dir.join("state.json"))?;
    let settings = store.data.settings.clone();

    let options = SessionOptions {
        fastresume: true,
        persistence: Some(SessionPersistenceConfig::Json {
            folder: Some(data_dir.join("session")),
        }),
        listen: Some(ListenerOptions {
            enable_upnp_port_forwarding: true,
            ..Default::default()
        }),
        ratelimits: LimitsConfig {
            download_bps: limit(settings.down_limit_kib),
            upload_bps: limit(settings.up_limit_kib),
        },
        client_name_and_version: Some(format!("RustTorrent {}", crate::VERSION)),
        ..Default::default()
    };
    let session = Session::new_with_opts(settings.download_dir.clone(), options)
        .await
        .map_err(|e| anyhow::anyhow!("The torrent engine could not start: {e:#}"))?;

    let (add_tx, mut add_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut w = Worker {
        session,
        store,
        log_path: data_dir.join("rusttorrent.log"),
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
    };
    if let Some(note) = note {
        w.notice(note, true);
    }
    // Links whose details had not arrived when the app last quit.
    for text in std::mem::take(&mut w.store.data.pending) {
        w.add(text);
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
            _ = timer.tick() => w.tick().await,
        }
    }

    for p in w.pending.drain(..) {
        p.task.abort();
    }
    w.collect_uploads();
    w.save();
    w.session.stop().await;
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
        match self.store.save() {
            Ok(()) => {
                self.dirty = false;
                self.last_save = Instant::now();
            }
            Err(e) => self.notice(format!("Could not save settings: {e:#}"), true),
        }
    }

    fn handles(&self) -> Vec<Handle> {
        self.session.with_torrents(|it| it.map(|(_, h)| h.clone()).collect())
    }

    fn find(&self, key: &str) -> Option<Handle> {
        self.handles().into_iter().find(|h| h.info_hash().as_string() == key)
    }

    fn add(&mut self, text: String) {
        let source = match intake::parse(&text) {
            Ok(s) => s,
            Err(e) => return self.notice(e, true),
        };
        let text = text.trim().to_owned();
        if self.pending.iter().any(|p| p.text == text) {
            return;
        }
        let label = intake::label(&source);
        let id = self.next_pending;
        self.next_pending += 1;

        let session = self.session.clone();
        let tx = self.add_tx.clone();
        let folder = self.store.data.settings.download_dir.to_string_lossy().into_owned();
        let (task_text, task_label) = (text.clone(), label.clone());
        let task = tokio::spawn(async move {
            let result = add_to_engine(&session, source, folder).await;
            let _ = tx.send(AddDone {
                id,
                text: task_text,
                label: task_label,
                result,
            });
        });
        self.pending.push(Pending {
            id,
            text: text.clone(),
            label,
            added_at: now_secs(),
            task,
        });
        self.store.data.pending.push(text);
        self.save();
    }

    fn added(&mut self, done: AddDone) {
        self.pending.retain(|p| p.id != done.id);
        self.store.data.pending.retain(|t| *t != done.text);
        match done.result {
            Ok((key, name, false)) => {
                let queue_pos = self.next_queue_pos();
                self.store.data.torrents.insert(
                    key,
                    Record {
                        intent: Intent::Run,
                        added_at: now_secs(),
                        queue_pos,
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
        }
    }

    fn set_intent(&mut self, keys: &[String], intent: Intent) {
        for key in keys {
            if let Some(record) = self.store.data.torrents.get_mut(key) {
                record.intent = intent;
            }
        }
        self.save();
    }

    async fn handle(&mut self, command: Command) {
        match command {
            Command::Add(text) => self.add(text),
            Command::Pause(keys) => self.set_intent(&keys, Intent::Pause),
            Command::Resume(keys) => self.set_intent(&keys, Intent::Run),
            Command::Remove { keys, delete_files } => {
                for key in keys {
                    if let Some(id) = key.strip_prefix("pending:").and_then(|n| n.parse::<u64>().ok()) {
                        if let Some(i) = self.pending.iter().position(|p| p.id == id) {
                            let p = self.pending.remove(i);
                            p.task.abort();
                            self.store.data.pending.retain(|t| *t != p.text);
                        }
                        continue;
                    }
                    let Some(handle) = self.find(&key) else { continue };
                    let name = handle.name().unwrap_or_else(|| key.clone());
                    match self
                        .session
                        .delete(TorrentIdOrHash::Id(handle.id()), delete_files)
                        .await
                    {
                        Ok(()) => {
                            self.store.data.torrents.remove(&key);
                            self.last_up.remove(&key);
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
                let Some(handle) = self.find(&key) else { return };
                let set: HashSet<usize> = files.into_iter().collect();
                if let Err(e) = self.session.update_only_files(&handle, &set).await {
                    let name = handle.name().unwrap_or(key);
                    self.notice(format!("Could not change the chosen files of {name}: {e:#}"), true);
                }
            }
            Command::SetLimits { down_kib, up_kib } => {
                self.store.data.settings.down_limit_kib = down_kib;
                self.store.data.settings.up_limit_kib = up_kib;
                self.session.ratelimits.set_download_bps(limit(down_kib));
                self.session.ratelimits.set_upload_bps(limit(up_kib));
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
            Command::Watch(key) => self.watched = key,
            Command::Shutdown => {}
        }
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
        let mut torrents = Vec::new();
        let (mut down_total, mut up_total) = (0, 0);

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
            self.count_upload(&key, live, stats.uploaded_bytes);
            rows.push((handle, key, stats));
        }
        let items: Vec<rules::Item> = rows.iter().map(|(_, key, stats)| self.item(key, stats)).collect();
        let now = chrono::Local::now();
        let clock = Clock {
            weekday: now.weekday().num_days_from_monday() as u8,
            minute: (now.hour() * 60 + now.minute()) as u16,
        };
        let outcomes = rules::apply(&items, &self.store.data.settings, clock);
        let schedule_off = !self.store.data.settings.schedule.allows(clock);

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
                TorrentStatsState::Live if !should_run => Some((self.session.pause(&handle).await, Intent::Run)),
                TorrentStatsState::Paused if should_run => Some((self.session.unpause(&handle).await, Intent::Pause)),
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
                key,
            });
        }

        for p in &self.pending {
            torrents.push(TorrentView {
                key: format!("pending:{}", p.id),
                name: p.label.clone(),
                state: State::FetchingInfo,
                error: None,
                total: 0,
                done: 0,
                uploaded: 0,
                down_bps: 0,
                up_bps: 0,
                peers_live: 0,
                peers_seen: 0,
                category: None,
                save_path: self.store.data.settings.download_dir.to_string_lossy().into_owned(),
                added_at: p.added_at,
                held: None,
                queue_pos: None,
                ratio_limit_pct: None,
                files: Arc::default(),
                file_done: Vec::new(),
                selected: None,
                trackers: Arc::default(),
                peers: Vec::new(),
            });
        }

        if self.dirty && self.last_save.elapsed() >= SAVE_EVERY {
            self.save();
        }

        let snapshot = Snapshot {
            ready: true,
            fatal: None,
            torrents,
            down_bps: down_total,
            up_bps: up_total,
            settings: self.store.data.settings.clone(),
            schedule_off,
            notices: self.notices.clone(),
        };
        let mut current = self.shared.lock().unwrap_or_else(|e| e.into_inner());
        if **current != snapshot {
            *current = Arc::new(snapshot);
            drop(current);
            (self.on_change)();
        }
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

async fn add_to_engine(
    session: &Arc<Session>,
    source: Source,
    folder: String,
) -> Result<(String, String, bool), String> {
    let add = match source {
        Source::Link(url) => AddTorrent::from_url(url),
        Source::File(path) => {
            let bytes = tokio::task::spawn_blocking(move || {
                std::fs::read(&path).map_err(|e| format!("could not read {}: {e}", path.display()))
            })
            .await
            .map_err(|e| e.to_string())??;
            AddTorrent::from_bytes(bytes)
        }
    };
    let options = AddTorrentOptions {
        overwrite: true,
        output_folder: Some(folder),
        ..Default::default()
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
