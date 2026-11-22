//! RustTorrent's core: every rule of the app, with no window code in it.
//!
//! A front end (the desktop window, the remote-control page) does two things:
//! it sends [`Command`]s and it reads the latest [`Snapshot`]. It never touches the
//! torrent engine directly.
pub mod arbiter;
pub mod blocklist;
pub mod comfort;
pub mod disk;
mod engine;
pub mod episodes;
pub mod feeds;
pub mod format;
pub mod import;
pub mod intake;
pub mod limits;
mod logfile;
pub mod make;
pub mod manage;
pub mod model;
pub mod natpmp;
pub mod net;
pub mod network;
pub mod priority;
pub mod relocate;
pub mod remote;
pub mod rules;
pub mod secrets;
pub mod store;
mod stream;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub use arbiter::HoldReason;
pub use comfort::{AllTime, ComfortSettings, Finished, PieceMap, SpeedHistory, Theme, WhenAllDone};
pub use engine::Command;
pub use feeds::Feed;
pub use limits::{LimitSettings, SeedStop, Seeding, SlowMode, SlowReason, TorrentLimits};
pub use make::{Details, MakeOptions, MakeView};
pub use manage::{AddOptions, Inspection};
pub use model::{FeedPreview, FeedView, FileMeta, Notice, PeerView, Snapshot, State, TorrentView};
pub use net::Interface;
pub use network::{NetworkSettings, NetworkStatus, ProxySettings};
pub use priority::{FilePriorities, Priority};
pub use rules::{Move, Schedule};
pub use store::{RemoteSettings, Settings};
pub use stream::StreamLink;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Makes ring the crypto library for every TLS connection. reqwest and librqbit are
/// built without one of their own, and reqwest panics if it builds a client with
/// none, so this runs before any session or HTTP client is made. Safe to call again.
pub(crate) fn use_ring() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

pub(crate) fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Where the app keeps its own files.
pub fn default_data_dir() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("RustTorrent")
}

pub(crate) type Shared = Arc<Mutex<Arc<Snapshot>>>;

pub struct Core {
    tx: tokio::sync::mpsc::UnboundedSender<Command>,
    shared: Shared,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Core {
    /// Starts the engine on its own thread. `on_change` is called whenever a new snapshot
    /// is ready, so a window can redraw only then.
    pub fn start(data_dir: PathBuf, on_change: impl Fn() + Send + Sync + 'static) -> Self {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let shared: Shared = Arc::new(Mutex::new(Arc::new(Snapshot::default())));
        let thread = {
            let shared = shared.clone();
            let tx = tx.clone();
            std::thread::Builder::new()
                .name("rusttorrent-core".into())
                .spawn(move || engine::run(data_dir, rx, tx, shared, Arc::new(on_change)))
                .expect("starting the core thread")
        };
        Self {
            tx,
            shared,
            thread: Some(thread),
        }
    }

    pub fn send(&self, command: Command) {
        let _ = self.tx.send(command);
    }

    pub fn snapshot(&self) -> Arc<Snapshot> {
        self.shared.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// A handle that other threads, such as AppleScript's, can keep to reach the engine.
    pub fn remote(&self) -> Remote {
        Remote {
            tx: self.tx.clone(),
            shared: self.shared.clone(),
        }
    }

    /// Saves everything and stops the engine. Waits a few seconds at most.
    pub fn shutdown(&mut self) {
        self.send(Command::Shutdown);
        if let Some(thread) = self.thread.take() {
            let deadline = Instant::now() + Duration::from_secs(5);
            while !thread.is_finished() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(20));
            }
            if thread.is_finished() {
                let _ = thread.join();
            }
        }
    }
}

/// Sends commands and reads the snapshot, like [`Core`], but cannot stop the engine.
#[derive(Clone)]
pub struct Remote {
    tx: tokio::sync::mpsc::UnboundedSender<Command>,
    shared: Shared,
}

impl Remote {
    pub fn send(&self, command: Command) {
        let _ = self.tx.send(command);
    }

    pub fn snapshot(&self) -> Arc<Snapshot> {
        self.shared.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

impl Drop for Core {
    fn drop(&mut self) {
        if self.thread.is_some() {
            self.shutdown();
        }
    }
}

/// macOS malloc keeps freed pages and counts them against the app. This hands them
/// back to the system; it takes well under a millisecond.
pub fn give_back_memory() {
    #[cfg(target_os = "macos")]
    {
        unsafe extern "C" {
            // libSystem.
            fn malloc_zone_pressure_relief(zone: *mut std::ffi::c_void, goal: usize) -> usize;
        }
        // A null zone means every zone; a goal of 0 means as much as possible.
        unsafe { malloc_zone_pressure_relief(std::ptr::null_mut(), 0) };
    }
}
