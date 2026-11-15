//! RustTorrent's core: every rule of the app, with no window code in it.
//!
//! A front end (the desktop window, the remote-control page) does two things:
//! it sends [`Command`]s and it reads the latest [`Snapshot`]. It never touches the
//! torrent engine directly.
pub mod arbiter;
mod engine;
pub mod feeds;
pub mod format;
pub mod intake;
pub mod model;
pub mod net;
pub mod remote;
pub mod rules;
pub mod store;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub use arbiter::HoldReason;
pub use engine::Command;
pub use feeds::Feed;
pub use model::{FeedView, FileMeta, Notice, PeerView, Snapshot, State, TorrentView};
pub use net::Interface;
pub use rules::{Move, Schedule};
pub use store::{RemoteSettings, Settings};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

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

impl Drop for Core {
    fn drop(&mut self) {
        if self.thread.is_some() {
            self.shutdown();
        }
    }
}
