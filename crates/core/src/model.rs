//! What a front end sees: a plain copy of the current state, with no engine types in it.
use std::sync::Arc;

use crate::arbiter::HoldReason;
use crate::feeds::Feed;
use crate::net::Interface;
use crate::store::Settings;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum State {
    /// A link was added and its details (name, files) have not arrived yet.
    FetchingInfo,
    /// Checking files already on disk.
    Checking,
    Downloading,
    Seeding,
    /// Paused before the download finished.
    Paused,
    /// Waiting for a free place among the downloads.
    Queued,
    /// Stopped by the schedule until transfers are allowed again.
    Scheduled,
    /// Finished and not seeding.
    Completed,
    /// Wants to run, but all traffic is blocked because the chosen network interface is
    /// not connected.
    Blocked,
    Error,
}

impl State {
    pub fn label(self) -> &'static str {
        match self {
            State::FetchingInfo => "Fetching info",
            State::Checking => "Checking",
            State::Downloading => "Downloading",
            State::Seeding => "Seeding",
            State::Paused => "Paused",
            State::Queued => "Queued",
            State::Scheduled => "Off by schedule",
            State::Completed => "Completed",
            State::Blocked => "Blocked",
            State::Error => "Error",
        }
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct FileMeta {
    pub name: String,
    pub len: u64,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct PeerView {
    pub addr: String,
    pub client: String,
    pub connection: &'static str,
    pub downloaded: u64,
    pub uploaded: u64,
}

#[derive(Clone, PartialEq, Debug)]
pub struct TorrentView {
    /// The torrent's info-hash in hex, or `pending:<n>` while details are being fetched.
    pub key: String,
    pub name: String,
    pub state: State,
    pub error: Option<String>,
    pub total: u64,
    pub done: u64,
    /// Uploaded over the torrent's whole life.
    pub uploaded: u64,
    pub down_bps: u64,
    pub up_bps: u64,
    pub peers_live: u32,
    pub peers_seen: u32,
    pub category: Option<String>,
    pub tags: Vec<String>,
    pub save_path: String,
    pub added_at: u64,
    /// Why the app is holding the torrent back although the user wants it running.
    pub held: Option<HoldReason>,
    /// Place in the download queue, from 1. `None` once finished.
    pub queue_pos: Option<u32>,
    /// This torrent's own ratio limit in percent, if it has one. 0 means no limit.
    pub ratio_limit_pct: Option<u32>,
    pub files: Arc<Vec<FileMeta>>,
    /// Bytes done per file, same order as `files`.
    pub file_done: Vec<u64>,
    /// Indexes of the files chosen for download. `None` means all of them.
    pub selected: Option<Vec<usize>>,
    pub trackers: Arc<Vec<String>>,
    /// Filled only for the torrent the front end is showing details for.
    pub peers: Vec<PeerView>,
}

impl TorrentView {
    pub fn progress(&self) -> f32 {
        if self.total == 0 {
            0.0
        } else {
            (self.done as f64 / self.total as f64) as f32
        }
    }

    pub fn is_pending(&self) -> bool {
        self.key.starts_with("pending:")
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Notice {
    /// Seconds since 1970.
    pub at: u64,
    pub text: String,
    pub error: bool,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct FeedView {
    pub feed: Feed,
    /// When the feed was last read, in seconds since 1970. 0 if never.
    pub checked_at: u64,
    pub error: Option<String>,
}

#[derive(Clone, PartialEq, Debug, Default)]
pub struct Snapshot {
    /// False until the engine has started.
    pub ready: bool,
    /// Set when the engine could not start at all. The app can show this and nothing else.
    pub fatal: Option<String>,
    pub torrents: Vec<TorrentView>,
    pub down_bps: u64,
    pub up_bps: u64,
    pub settings: Settings,
    /// The schedule is stopping all transfers right now.
    pub schedule_off: bool,
    pub feeds: Vec<FeedView>,
    /// Recent messages, oldest first.
    pub notices: Vec<Notice>,
    /// Set while the engine is stopped and nothing is sent or received: says why.
    pub blocked: Option<String>,
    /// The network interfaces that are connected right now.
    pub interfaces: Vec<Interface>,
    /// The remote-control page is being served right now.
    pub remote_on: bool,
    /// Why the remote-control page could not be served.
    pub remote_error: Option<String>,
}
