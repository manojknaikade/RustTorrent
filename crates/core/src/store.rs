//! The app's own saved state: settings and one record per torrent, keyed by info-hash.
//!
//! The engine keeps what it needs to resume downloads. Everything the engine does not
//! know about (what the user asked for, categories, lifetime upload totals) lives here,
//! in one JSON file with a version number, written so a crash cannot leave half a file.
use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context};
use serde::{Deserialize, Serialize};

use crate::arbiter::Intent;
use crate::feeds::Feed;
use crate::limits::{LimitSettings, Seeding};
use crate::rules::Schedule;

pub const SCHEMA_VERSION: u32 = 1;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(default)]
pub struct Settings {
    pub download_dir: PathBuf,
    /// Unfinished downloads are kept here, and each is moved to its own folder once it
    /// finishes. `None` downloads straight into that folder.
    pub incomplete_dir: Option<PathBuf>,
    /// Pause a download when less than this many MiB is free on its disk. 0 is off.
    pub keep_free_mib: u64,
    /// Download limit in KiB/s. 0 means no limit.
    pub down_limit_kib: u32,
    /// Upload limit in KiB/s. 0 means no limit.
    pub up_limit_kib: u32,
    /// How many torrents may download at once. 0 means no limit.
    pub max_downloads: u32,
    /// Stop seeding at this ratio, in percent (150 is a ratio of 1.5). 0 means no limit.
    pub ratio_limit_pct: u32,
    pub schedule: Schedule,
    /// `.torrent` files put in these folders are added by themselves.
    pub watch_dirs: Vec<WatchFolder>,
    /// The single watch folder of versions before 0.14. Read once, moved into
    /// `watch_dirs` on load, never written.
    #[serde(skip_serializing)]
    pub watch_dir: Option<PathBuf>,
    /// Send and receive everything through this network interface alone, and nothing at
    /// all while it is not connected. `None` uses whatever connection the system has.
    pub bind_interface: Option<String>,
    pub remote: RemoteSettings,
    /// Show the add dialog (folder, category, files) before a torrent added from the
    /// window starts. Watch folder, feeds and the remote page never ask.
    pub ask_before_add: bool,
    /// The folder each category saves to, by category name. Others use `download_dir`.
    pub category_dirs: BTreeMap<String, PathBuf>,
    /// Every category made so far, kept even while no torrent is in it.
    pub categories: std::collections::BTreeSet<String>,
    /// Slow mode, seeding time limits, queue limits and peers per torrent.
    pub limits: LimitSettings,
    /// Port, peer finding, proxy and block list. Applied by starting a new engine session.
    pub network: crate::network::NetworkSettings,
    pub comfort: crate::comfort::ComfortSettings,
    /// Folders chosen lately, newest first, offered in the folder menus.
    pub recent_dirs: Vec<PathBuf>,
}

impl Settings {
    /// How many folders [`Settings::recent_dirs`] keeps.
    pub const RECENT_DIRS: usize = 8;

    /// Puts `dir` first in the recent folders, without repeats.
    pub fn remember_dir(&mut self, dir: &std::path::Path) {
        self.recent_dirs.retain(|d| d != dir);
        self.recent_dirs.insert(0, dir.to_path_buf());
        self.recent_dirs.truncate(Self::RECENT_DIRS);
    }
}

/// Remote control from a browser. Off until the user turns it on and sets a password.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(default)]
pub struct RemoteSettings {
    pub enabled: bool,
    pub port: u16,
    /// False: only this computer can open the page. True: other devices on the same
    /// network can too.
    pub lan: bool,
    /// The password is never kept, only this salt and the hash made with it, in hex.
    pub salt: String,
    pub hash: String,
    /// Serve the page over HTTPS with a certificate made on this computer.
    pub https: bool,
}

impl RemoteSettings {
    pub fn has_password(&self) -> bool {
        !self.hash.is_empty()
    }
}

impl Default for RemoteSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            port: 8787,
            lan: false,
            salt: String::new(),
            hash: String::new(),
            https: false,
        }
    }
}

impl Default for Settings {
    fn default() -> Self {
        let download_dir = dirs::download_dir()
            .or_else(|| dirs::home_dir().map(|h| h.join("Downloads")))
            .unwrap_or_else(|| PathBuf::from("."));
        Self {
            download_dir,
            incomplete_dir: None,
            keep_free_mib: 1024,
            down_limit_kib: 0,
            up_limit_kib: 0,
            max_downloads: 0,
            ratio_limit_pct: 0,
            schedule: Schedule::default(),
            watch_dirs: Vec::new(),
            watch_dir: None,
            bind_interface: None,
            remote: RemoteSettings::default(),
            ask_before_add: true,
            category_dirs: BTreeMap::new(),
            categories: Default::default(),
            limits: LimitSettings::default(),
            network: Default::default(),
            comfort: crate::comfort::ComfortSettings::default(),
            recent_dirs: Vec::new(),
        }
    }
}

/// A folder whose `.torrent` files are added by themselves, with the category they get.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
#[serde(default)]
pub struct WatchFolder {
    pub dir: PathBuf,
    pub category: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
#[serde(default)]
pub struct Record {
    pub intent: Intent,
    pub category: Option<String>,
    /// Sorted, without repeats.
    pub tags: Vec<String>,
    /// Bytes uploaded over the torrent's whole life, across restarts.
    pub uploaded: u64,
    /// Seconds since 1970.
    pub added_at: u64,
    /// Place in the download queue; lower downloads first. 0 for torrents added before
    /// the queue existed, which are ordered by `added_at`.
    pub queue_pos: u64,
    /// This torrent's own ratio limit in percent, instead of the one in the settings.
    /// 0 means no limit.
    pub ratio_limit_pct: Option<u32>,
    /// Name, size and progress as last seen, so the list can be shown while the engine
    /// is not running.
    pub name: String,
    pub total: u64,
    pub done: u64,
    pub finished: bool,
    /// Downloads even when the queue is full. The schedule and the network interface
    /// still apply.
    pub force_start: bool,
    /// How long it has seeded, and its own seeding time limits.
    pub seeding: Seeding,
    /// This torrent's own speed limits in KiB/s. 0 means no limit of its own.
    pub down_limit_kib: u32,
    pub up_limit_kib: u32,
    /// Files set to high or low priority.
    pub priorities: crate::priority::FilePriorities,
    /// A move of its data to another folder that has not ended yet.
    pub moving: Option<crate::relocate::MoveNote>,
    /// Where the data goes once the download finishes, while it is kept in the folder
    /// for unfinished downloads. See [`Settings::incomplete_dir`].
    pub final_dir: Option<PathBuf>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
#[serde(default)]
pub struct FeedState {
    /// Items already handed to the downloader, so they are not added twice.
    pub seen: Vec<String>,
    /// When the feed was last read, in seconds since 1970.
    pub checked_at: u64,
    /// Why the last read failed, if it did.
    pub error: Option<String>,
    /// Seasons and episodes already taken, such as "S02E05", for feeds that skip repeats.
    pub episodes: Vec<String>,
    /// The feed has been read once since "only newer items" was turned on.
    pub primed: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(default)]
pub struct Data {
    pub schema_version: u32,
    pub settings: Settings,
    pub torrents: BTreeMap<String, Record>,
    /// Links that were added but whose details had not arrived when the app last quit.
    pub pending: Vec<String>,
    pub feeds: Vec<Feed>,
    /// What the app remembers about each feed, by the feed's address.
    pub feed_state: BTreeMap<String, FeedState>,
    /// Totals over the app's whole life.
    pub all_time: crate::comfort::AllTime,
}

impl Default for Data {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            settings: Settings::default(),
            torrents: BTreeMap::new(),
            pending: Vec::new(),
            feeds: Vec::new(),
            feed_state: BTreeMap::new(),
            all_time: crate::comfort::AllTime::default(),
        }
    }
}

pub struct Store {
    path: PathBuf,
    pub data: Data,
}

impl Store {
    /// Loads the file, or starts empty if there is none.
    ///
    /// A file that cannot be read is moved aside (never deleted) and the returned note
    /// says where. A file written by a newer version of the app is an error: this
    /// version must not overwrite it.
    pub fn load(path: &Path) -> anyhow::Result<(Self, Option<String>)> {
        let fresh = |note| {
            Ok((
                Self {
                    path: path.to_owned(),
                    data: Data::default(),
                },
                note,
            ))
        };
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return fresh(None),
            Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
        };
        match serde_json::from_str::<Data>(&text) {
            Ok(data) if data.schema_version > SCHEMA_VERSION => bail!(
                "{} was written by a newer version of RustTorrent (format {}, this version reads up to {}). \
                 Update the app, or move that file away to start fresh.",
                path.display(),
                data.schema_version,
                SCHEMA_VERSION
            ),
            Ok(mut data) => {
                // Versions before 0.14 had one watch folder.
                if let Some(dir) = data.settings.watch_dir.take() {
                    if data.settings.watch_dirs.is_empty() {
                        data.settings.watch_dirs.push(WatchFolder { dir, category: None });
                    }
                }
                Ok((
                    Self {
                        path: path.to_owned(),
                        data,
                    },
                    None,
                ))
            }
            Err(e) => {
                let aside = path.with_extension(format!("unreadable-{}.json", crate::now_secs()));
                std::fs::rename(path, &aside).with_context(|| format!("moving unreadable {} aside", path.display()))?;
                fresh(Some(format!(
                    "Saved settings could not be read ({e}). They were moved to {} and the app started with defaults.",
                    aside.display()
                )))
            }
        }
    }

    /// Writes to a temporary file, flushes it to disk, then renames it over the old one.
    pub fn save(&self) -> anyhow::Result<()> {
        let tmp = self.path.with_extension("json.tmp");
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let mut f = std::fs::File::create(&tmp).with_context(|| format!("creating {}", tmp.display()))?;
        f.write_all(&serde_json::to_vec_pretty(&self.data)?)?;
        f.sync_all()?;
        drop(f);
        std::fs::rename(&tmp, &self.path).with_context(|| format!("replacing {}", self.path.display()))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("rusttorrent-store-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("state.json")
    }

    #[test]
    fn recent_dirs_newest_first_without_repeats() {
        let mut s = Settings::default();
        for i in 0..10 {
            s.remember_dir(&PathBuf::from(format!("/d{i}")));
        }
        s.remember_dir(&PathBuf::from("/d5"));
        assert_eq!(s.recent_dirs.len(), Settings::RECENT_DIRS);
        assert_eq!(s.recent_dirs[0], PathBuf::from("/d5"));
        assert_eq!(s.recent_dirs[1], PathBuf::from("/d9"));
        assert_eq!(s.recent_dirs.iter().filter(|d| d.ends_with("d5")).count(), 1);
    }

    #[test]
    fn missing_file_gives_defaults() {
        let (store, note) = Store::load(&temp("missing")).unwrap();
        assert!(note.is_none());
        assert_eq!(store.data, Data::default());
    }

    #[test]
    fn round_trip() {
        let path = temp("roundtrip");
        let (mut store, _) = Store::load(&path).unwrap();
        store.data.settings.down_limit_kib = 512;
        store.data.settings.max_downloads = 2;
        store.data.settings.schedule.enabled = true;
        store.data.pending.push("magnet:?xt=urn:btih:abc".into());
        store.data.settings.watch_dirs.push(WatchFolder {
            dir: PathBuf::from("/tmp/watch"),
            category: Some("Linux".into()),
        });
        store.data.settings.incomplete_dir = Some(PathBuf::from("/tmp/unfinished"));
        store.data.settings.bind_interface = Some("utun4".into());
        store.data.settings.ask_before_add = false;
        store
            .data
            .settings
            .category_dirs
            .insert("Linux".into(), PathBuf::from("/tmp/linux"));
        store.data.feeds.push(Feed {
            url: "https://example.org/feed".into(),
            include: "debian".into(),
            ..Default::default()
        });
        store.data.feed_state.insert(
            "https://example.org/feed".into(),
            FeedState {
                seen: vec!["id-1".into()],
                checked_at: 9,
                error: None,
                ..Default::default()
            },
        );
        store.data.torrents.insert(
            "00112233".into(),
            Record {
                intent: Intent::Pause,
                category: Some("Linux".into()),
                tags: vec!["iso".into()],
                uploaded: 42,
                added_at: 7,
                queue_pos: 3,
                ratio_limit_pct: Some(200),
                name: "debian.iso".into(),
                total: 100,
                done: 60,
                finished: false,
                force_start: true,
                seeding: Seeding {
                    time_limit_min: Some(30),
                    idle_limit_min: None,
                    secs: 90,
                    idle_secs: 12,
                },
                down_limit_kib: 500,
                up_limit_kib: 50,
                priorities: crate::priority::FilePriorities {
                    high: vec![1],
                    low: vec![0, 2],
                },
                moving: Some(crate::relocate::MoveNote {
                    from: "/a".into(),
                    to: "/b".into(),
                    files: vec!["x/y.iso".into()],
                    only_files: Some(vec![0]),
                    announce: true,
                }),
                final_dir: Some("/done".into()),
            },
        );
        store.save().unwrap();
        let (again, note) = Store::load(&path).unwrap();
        assert!(note.is_none());
        assert_eq!(again.data, store.data);
        assert!(!path.with_extension("json.tmp").exists());
    }

    #[test]
    fn unreadable_file_is_moved_aside_not_deleted() {
        let path = temp("corrupt");
        std::fs::write(&path, "{ not json").unwrap();
        let (store, note) = Store::load(&path).unwrap();
        assert!(note.is_some());
        assert_eq!(store.data, Data::default());
        assert!(!path.exists());
        let kept = std::fs::read_dir(path.parent().unwrap()).unwrap().count();
        assert_eq!(kept, 1, "the unreadable file is kept under another name");
    }

    #[test]
    fn newer_format_is_refused() {
        let path = temp("newer");
        std::fs::write(&path, format!("{{\"schema_version\": {}}}", SCHEMA_VERSION + 1)).unwrap();
        assert!(Store::load(&path).is_err());
        assert!(path.exists(), "the newer file is left untouched");
    }

    #[test]
    fn unknown_and_missing_fields_are_tolerated() {
        let path = temp("fields");
        std::fs::write(
            &path,
            r#"{"schema_version":1,"future_field":true,"torrents":{"aa":{}}}"#,
        )
        .unwrap();
        let (store, note) = Store::load(&path).unwrap();
        assert!(note.is_none());
        assert_eq!(store.data.torrents["aa"], Record::default());
        assert!(store.data.settings.ask_before_add, "files from before 0.7 still ask");
    }

    #[test]
    fn old_watch_folder_moves_into_the_list() {
        let path = temp("watch");
        std::fs::write(&path, r#"{"schema_version":1,"settings":{"watch_dir":"/tmp/w"}}"#).unwrap();
        let (store, _) = Store::load(&path).unwrap();
        let folders = &store.data.settings.watch_dirs;
        assert_eq!(
            folders,
            &[WatchFolder {
                dir: PathBuf::from("/tmp/w"),
                category: None
            }]
        );
        assert_eq!(store.data.settings.watch_dir, None);
        store.save().unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains("\"watch_dir\""), "the old field is not written again");
    }
}
