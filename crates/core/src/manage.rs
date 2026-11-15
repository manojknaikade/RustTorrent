//! Adding with choices made first: the save folder, the category, which files, and
//! whether to start paused. Also the file list shown before adding, and the folder each
//! category saves to.
//!
//! The file list comes from the engine's "list only" add, which fetches a magnet link's
//! details (or reads a `.torrent` file) without starting the download. The details it
//! fetched are kept, so adding afterwards does not have to fetch them a second time.
use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use librqbit::{AddTorrent, AddTorrentOptions, AddTorrentResponse, Magnet, Session};

use crate::intake::{self, Source};
use crate::model::FileMeta;
use crate::store::Settings;

/// What the user chose in the add dialog.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AddOptions {
    /// A magnet link, web address, file address or file path. See [`crate::intake`].
    pub text: String,
    /// Where to save. `None` uses the category's folder, or else the download folder.
    pub folder: Option<PathBuf>,
    pub category: Option<String>,
    pub paused: bool,
    /// Indexes of the files to download. `None` means all of them.
    pub files: Option<Vec<usize>>,
}

/// A torrent's name and files, fetched before it is added.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Inspection {
    /// The text it was asked for with, as sent.
    pub text: String,
    /// False while the details are still being fetched.
    pub ready: bool,
    pub name: String,
    pub files: Vec<FileMeta>,
    pub error: Option<String>,
}

/// The folder a torrent is saved to: the one chosen, else its category's, else the
/// download folder.
pub fn folder_for(settings: &Settings, chosen: Option<&PathBuf>, category: Option<&str>) -> PathBuf {
    chosen
        .cloned()
        .or_else(|| category.and_then(|c| settings.category_dirs.get(c).cloned()))
        .unwrap_or_else(|| settings.download_dir.clone())
}

/// A magnet link for a torrent in the list: its info-hash, name and trackers.
pub fn magnet_link(t: &crate::model::TorrentView) -> String {
    let mut link = format!("magnet:?xt=urn:btih:{}", t.key);
    if !t.name.is_empty() && t.name != t.key {
        link.push_str("&dn=");
        link.push_str(&escape(&t.name));
    }
    for tracker in t.trackers.iter() {
        link.push_str("&tr=");
        link.push_str(&escape(tracker));
    }
    link
}

/// Percent-encodes everything but letters, digits and `-._~`.
fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for b in text.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// The engine options for adding with these choices.
pub(crate) fn engine_options(folder: &std::path::Path, paused: bool, files: Option<Vec<usize>>) -> AddTorrentOptions {
    AddTorrentOptions {
        overwrite: true,
        output_folder: Some(folder.to_string_lossy().into_owned()),
        paused,
        only_files: files,
        ..Default::default()
    }
}

/// Details fetched earlier, so adding does not fetch them again.
pub(crate) struct Prepared {
    pub bytes: Vec<u8>,
    /// A magnet link's trackers, which are not in the torrent's own bytes.
    pub trackers: Vec<String>,
    pub peers: Vec<SocketAddr>,
}

impl Prepared {
    pub fn into_add(self, options: &mut AddTorrentOptions) -> AddTorrent<'static> {
        if !self.trackers.is_empty() {
            options.trackers = Some(self.trackers);
        }
        if !self.peers.is_empty() {
            options.initial_peers = Some(self.peers);
        }
        AddTorrent::from_bytes(self.bytes)
    }
}

struct Found {
    view: Inspection,
    prepared: Option<Prepared>,
}

/// The torrents whose details the add dialog is waiting for.
#[derive(Default)]
pub(crate) struct Inspector {
    order: Vec<String>,
    found: Arc<Mutex<HashMap<String, Found>>>,
    tasks: HashMap<String, tokio::task::JoinHandle<()>>,
}

impl Inspector {
    /// Fetches details for these texts and forgets every other one.
    pub fn want(&mut self, texts: Vec<String>, session: Option<&Arc<Session>>) {
        let texts: Vec<String> = texts.into_iter().map(|t| t.trim().to_owned()).collect();
        self.tasks.retain(|text, task| {
            let keep = texts.contains(text);
            if !keep {
                task.abort();
            }
            keep
        });
        lock(&self.found).retain(|text, _| texts.contains(text));
        for text in &texts {
            if self.tasks.contains_key(text) || lock(&self.found).contains_key(text) {
                continue;
            }
            let mut view = Inspection {
                text: text.clone(),
                ready: false,
                name: label_of(text),
                files: Vec::new(),
                error: None,
            };
            let Some(session) = session.cloned() else {
                view.ready = true;
                view.error = Some("The file list cannot be fetched while all traffic is blocked.".into());
                lock(&self.found).insert(text.clone(), Found { view, prepared: None });
                continue;
            };
            lock(&self.found).insert(text.clone(), Found { view, prepared: None });
            let (found, key) = (self.found.clone(), text.clone());
            let task = tokio::spawn(async move {
                let result = inspect(&session, &key).await;
                if let Some(entry) = lock(&found).get_mut(&key) {
                    entry.view.ready = true;
                    match result {
                        Ok((name, files, prepared)) => {
                            entry.view.name = name;
                            entry.view.files = files;
                            entry.prepared = Some(prepared);
                        }
                        Err(e) => entry.view.error = Some(e),
                    }
                }
            });
            self.tasks.insert(text.clone(), task);
        }
        self.order = texts;
    }

    /// What has been found so far, in the order asked for.
    pub fn views(&self) -> Vec<Inspection> {
        let found = lock(&self.found);
        self.order
            .iter()
            .filter_map(|t| found.get(t).map(|f| f.view.clone()))
            .collect()
    }

    /// Takes the fetched details for this text, if they arrived, and forgets it.
    pub fn take(&mut self, text: &str) -> Option<Prepared> {
        let text = text.trim();
        if let Some(task) = self.tasks.remove(text) {
            task.abort();
        }
        self.order.retain(|t| t != text);
        lock(&self.found).remove(text).and_then(|f| f.prepared)
    }

    /// Stops all fetching, for when the engine stops.
    pub fn stop(&mut self) {
        for (_, task) in self.tasks.drain() {
            task.abort();
        }
        lock(&self.found).clear();
        self.order.clear();
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn label_of(text: &str) -> String {
    intake::parse(text)
        .map(|s| intake::label(&s))
        .unwrap_or_else(|_| text.to_owned())
}

async fn inspect(session: &Arc<Session>, text: &str) -> Result<(String, Vec<FileMeta>, Prepared), String> {
    let mut trackers = Vec::new();
    let add = match intake::parse(text)? {
        Source::Link(url) => {
            if url.to_ascii_lowercase().starts_with("magnet:") {
                trackers = Magnet::parse(&url).map(|m| m.trackers).unwrap_or_default();
            }
            AddTorrent::from_url(url)
        }
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
        list_only: true,
        ..Default::default()
    };
    let r = match session
        .add_torrent(add, Some(options))
        .await
        .map_err(|e| format!("{e:#}"))?
    {
        AddTorrentResponse::ListOnly(r) => r,
        _ => return Err("the engine did not list the files".into()),
    };
    let files: Vec<FileMeta> = r
        .info
        .iter_file_details()
        .map(|f| FileMeta {
            name: f.filename.to_pathbuf().to_string_lossy().into_owned(),
            len: f.len,
        })
        .collect();
    let name = r.info.name().map(|n| n.into_owned()).unwrap_or_else(|| label_of(text));
    let prepared = Prepared {
        bytes: r.torrent_bytes.to_vec(),
        trackers,
        peers: r.seen_peers,
    };
    Ok((name, files, prepared))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folder_choice_order() {
        let mut settings = Settings {
            download_dir: PathBuf::from("/d"),
            ..Default::default()
        };
        settings.category_dirs.insert("Linux".into(), PathBuf::from("/linux"));
        let chosen = PathBuf::from("/chosen");
        assert_eq!(folder_for(&settings, Some(&chosen), Some("Linux")), chosen);
        assert_eq!(folder_for(&settings, None, Some("Linux")), PathBuf::from("/linux"));
        assert_eq!(folder_for(&settings, None, Some("Other")), PathBuf::from("/d"));
        assert_eq!(folder_for(&settings, None, None), PathBuf::from("/d"));
    }

    #[test]
    fn magnet_link_has_hash_name_and_trackers() {
        let t = crate::model::TorrentView {
            key: "0123456789abcdef0123456789abcdef01234567".into(),
            name: "debian 13.iso".into(),
            state: crate::model::State::Seeding,
            error: None,
            total: 0,
            done: 0,
            uploaded: 0,
            down_bps: 0,
            up_bps: 0,
            peers_live: 0,
            peers_seen: 0,
            category: None,
            tags: Vec::new(),
            save_path: String::new(),
            added_at: 0,
            held: None,
            queue_pos: None,
            ratio_limit_pct: None,
            files: Arc::default(),
            file_done: Vec::new(),
            selected: None,
            trackers: Arc::new(vec!["http://t.example/announce?a=1".into()]),
            peers: Vec::new(),
            force_start: false,
        };
        assert_eq!(
            magnet_link(&t),
            "magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567&dn=debian%2013.iso&tr=http%3A%2F%2Ft.example%2Fannounce%3Fa%3D1"
        );
        // Every link the app makes can be read back as a magnet link.
        let parsed = Magnet::parse(&magnet_link(&t)).unwrap();
        assert_eq!(parsed.trackers, vec!["http://t.example/announce?a=1".to_owned()]);
    }

    #[test]
    fn engine_options_carry_the_choices() {
        let o = engine_options(std::path::Path::new("/x"), true, Some(vec![0, 2]));
        assert!(o.paused && o.overwrite && !o.list_only);
        assert_eq!(o.output_folder.as_deref(), Some("/x"));
        assert_eq!(o.only_files, Some(vec![0, 2]));
    }
}
