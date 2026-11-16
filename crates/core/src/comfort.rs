//! Things that make the app pleasant to live with: a short history of speeds, totals
//! over the app's whole life, knowing when a download finishes, and what to do then.
//!
//! The engine worker owns one [`Tracker`] and feeds it what it sees each second. Nothing
//! here talks to the engine except [`piece_map`].
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use serde::{Deserialize, Serialize};

use crate::model::{State, TorrentView};

/// How far back the speed history goes, in seconds.
pub const HISTORY_SECS: u64 = 300;
/// How many finished downloads the snapshot remembers.
const KEEP_FINISHED: usize = 20;
/// The most cells a piece map has, however many pieces the torrent has.
pub const MAP_CELLS: usize = 400;

#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Theme {
    Dark,
    Light,
    /// Follow the system.
    #[default]
    System,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WhenAllDone {
    #[default]
    Nothing,
    Quit,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(default)]
pub struct ComfortSettings {
    /// Show a system notification when a download finishes.
    pub notify_finished: bool,
    /// Keep the computer from going to sleep while anything is being sent or received.
    pub keep_awake: bool,
    pub theme: Theme,
    /// A program to run when a download finishes, with its arguments, as the user typed it.
    /// The torrent's name and folder are passed to it. Empty runs nothing.
    pub on_finish_program: String,
    pub when_all_done: WhenAllDone,
    /// Columns of the torrent list the user has hidden, by their title.
    pub hidden_columns: Vec<String>,
}

impl Default for ComfortSettings {
    fn default() -> Self {
        Self {
            notify_finished: true,
            keep_awake: true,
            theme: Theme::System,
            on_finish_program: String::new(),
            when_all_done: WhenAllDone::Nothing,
            hidden_columns: Vec::new(),
        }
    }
}

/// Totals over the app's whole life, kept in the state file.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[serde(default)]
pub struct AllTime {
    pub downloaded: u64,
    pub uploaded: u64,
    /// Seconds the app has been running.
    pub running_secs: u64,
}

/// Total download and upload speed over the last few minutes. Only changes are kept, so
/// the memory used is fixed and an idle app does not change at all.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SpeedHistory {
    /// (second, bytes/s down, bytes/s up), oldest first, one entry per change.
    samples: VecDeque<(u64, u64, u64)>,
}

impl SpeedHistory {
    pub fn push(&mut self, at: u64, down: u64, up: u64) {
        match self.samples.back_mut() {
            Some(&mut (_, d, u)) if d == down && u == up => return,
            Some(last) if last.0 == at => *last = (at, down, up),
            _ => self.samples.push_back((at, down, up)),
        }
        // Keep the newest sample older than the window: it tells the speed at its start.
        while self.samples.len() > 1 && self.samples[1].0 + HISTORY_SECS <= at {
            self.samples.pop_front();
        }
        while self.samples.len() > HISTORY_SECS as usize + 1 {
            self.samples.pop_front();
        }
    }

    /// The speed in each of the last `secs` seconds up to `now`, oldest first.
    pub fn series(&self, now: u64, secs: u64) -> Vec<(u64, u64)> {
        let mut out = Vec::with_capacity(secs as usize);
        let mut i = 0;
        let mut current = (0, 0);
        for at in now.saturating_sub(secs.saturating_sub(1))..=now {
            while i < self.samples.len() && self.samples[i].0 <= at {
                current = (self.samples[i].1, self.samples[i].2);
                i += 1;
            }
            out.push(current);
        }
        out
    }
}

/// One download that finished while the app was running.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Finished {
    /// Counts up from 1, so a front end can tell which ones it has already seen.
    pub id: u64,
    pub key: String,
    pub name: String,
    pub folder: String,
}

/// Which pieces of one torrent are done, squeezed into at most [`MAP_CELLS`] cells.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PieceMap {
    pub key: String,
    pub pieces: u32,
    /// How much of each cell is done, 0 to 255.
    pub cells: Vec<u8>,
}

/// Squeezes one flag per piece into at most `cells` cells.
pub fn squeeze(have: &[bool], cells: usize) -> Vec<u8> {
    let n = have.len();
    let cells = cells.min(n);
    (0..cells)
        .map(|c| {
            let (start, end) = (c * n / cells, ((c + 1) * n / cells).max(c * n / cells + 1));
            let done = have[start..end].iter().filter(|&&h| h).count();
            (done * 255 / (end - start)) as u8
        })
        .collect()
}

/// The piece map of a torrent, if the engine has it (it does once the torrent's details
/// are known, whether it is running or paused).
pub(crate) fn piece_map(session: &Arc<librqbit::Session>, handle: &librqbit::ManagedTorrent) -> Option<PieceMap> {
    let api = librqbit::Api::new(session.clone(), None);
    let (bits, len) = api
        .api_dump_haves(librqbit::api::TorrentIdOrHash::Hash(handle.info_hash()))
        .ok()?;
    let have: Vec<bool> = (0..len as usize).map(|i| bits.get(i).is_some_and(|b| *b)).collect();
    Some(PieceMap {
        key: handle.info_hash().as_string(),
        pieces: len,
        cells: squeeze(&have, MAP_CELLS),
    })
}

/// Splits a command line into the program and its arguments, the way a shell would for
/// plain words, double quotes, single quotes and backslashes. Nothing else is special:
/// the line is never given to a shell.
pub fn split_command(line: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut in_word = false;
    let mut chars = line.chars();
    let mut quote: Option<char> = None;
    while let Some(c) = chars.next() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some('"'), '\\') => match chars.next() {
                Some(n @ ('"' | '\\')) => word.push(n),
                Some(n) => {
                    word.push('\\');
                    word.push(n);
                }
                None => word.push('\\'),
            },
            (Some(_), c) => word.push(c),
            (None, '"' | '\'') => {
                quote = Some(c);
                in_word = true;
            }
            (None, '\\') => {
                if let Some(n) = chars.next() {
                    word.push(n);
                }
                in_word = true;
            }
            (None, c) if c.is_whitespace() => {
                if in_word {
                    words.push(std::mem::take(&mut word));
                    in_word = false;
                }
            }
            (None, c) => {
                word.push(c);
                in_word = true;
            }
        }
    }
    if in_word {
        words.push(word);
    }
    words
}

/// The program and arguments to run when a download finishes. `%N` and `%F` in an
/// argument are replaced by the torrent's name and folder; if neither appears anywhere,
/// the name and folder are added at the end.
pub fn finish_command(line: &str, name: &str, folder: &str) -> Vec<String> {
    let words = split_command(line);
    if words.is_empty() {
        return words;
    }
    if words.iter().any(|w| w.contains("%N") || w.contains("%F")) {
        words
            .into_iter()
            .map(|w| w.replace("%N", name).replace("%F", folder))
            .collect()
    } else {
        let mut words = words;
        words.push(name.to_owned());
        words.push(folder.to_owned());
        words
    }
}

/// Starts the program the user chose for finished downloads. Does not wait for it.
pub(crate) fn run_finish_program(line: &str, name: &str, folder: &Path) -> Result<(), String> {
    let words = finish_command(line, name, &folder.to_string_lossy());
    let Some((program, args)) = words.split_first() else {
        return Ok(());
    };
    let mut child = std::process::Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| format!("Could not run {program}: {e}"))?;
    // Collect it when it ends so it does not linger.
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

/// True when nothing is left to download: no torrent is downloading, getting its details,
/// being checked, or waiting its turn with parts still missing.
pub fn nothing_left(torrents: &[TorrentView]) -> bool {
    !torrents.iter().any(|t| match t.state {
        State::Downloading | State::FetchingInfo | State::Checking => true,
        State::Queued | State::Scheduled | State::Blocked => t.done < t.total || t.total == 0,
        _ => false,
    })
}

/// What the engine worker keeps for this module.
pub(crate) struct Tracker {
    pub speeds: SpeedHistory,
    /// Downloaded counter last seen per running torrent, to add only the difference.
    last_down: HashMap<String, u64>,
    /// Torrents seen downloading while the app runs; only these can "finish".
    downloading: HashSet<String>,
    pub finished: Vec<Finished>,
    next_id: u64,
    /// A download finished since "quit when all downloads finish" was last turned on.
    armed: bool,
    started: Instant,
    started_at: u64,
    /// Running time saved before this run.
    base_secs: u64,
}

impl Tracker {
    pub fn new(all_time: &AllTime) -> Self {
        Self {
            speeds: SpeedHistory::default(),
            last_down: HashMap::new(),
            downloading: HashSet::new(),
            finished: Vec::new(),
            next_id: 1,
            armed: false,
            started: Instant::now(),
            started_at: crate::now_secs(),
            base_secs: all_time.running_secs,
        }
    }

    /// Running time saved before this run, and when this run started.
    pub fn running(&self) -> (u64, u64) {
        (self.base_secs, self.started_at)
    }

    pub fn running_secs(&self) -> u64 {
        self.base_secs + self.started.elapsed().as_secs()
    }

    /// The difference since the last look at a running torrent's downloaded counter,
    /// which starts again from zero whenever the torrent restarts.
    pub fn count_download(&mut self, key: &str, current: Option<u64>) -> u64 {
        let Some(current) = current else {
            self.last_down.remove(key);
            return 0;
        };
        let last = self.last_down.insert(key.to_owned(), current).unwrap_or(0);
        if current >= last {
            current - last
        } else {
            current
        }
    }

    pub fn forget_counters(&mut self) {
        self.last_down.clear();
    }

    /// Notes what a torrent is doing; returns true the moment a download finishes.
    pub fn check_finish(&mut self, key: &str, live: bool, finished: bool) -> bool {
        if !live {
            return false;
        }
        if !finished {
            self.downloading.insert(key.to_owned());
            false
        } else {
            self.downloading.remove(key)
        }
    }

    pub fn add_finished(&mut self, key: String, name: String, folder: String) {
        self.finished.push(Finished {
            id: self.next_id,
            key,
            name,
            folder,
        });
        self.next_id += 1;
        self.armed = true;
        if self.finished.len() > KEEP_FINISHED {
            self.finished.remove(0);
        }
    }

    /// Called when the user turns "quit when all downloads finish" on: only downloads
    /// that finish after that count.
    pub fn disarm(&mut self) {
        self.armed = false;
    }

    /// The app should quit now.
    pub fn should_quit(&self, when: WhenAllDone, torrents: &[TorrentView]) -> bool {
        when == WhenAllDone::Quit && self.armed && nothing_left(torrents)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn history_keeps_changes_only() {
        let mut h = SpeedHistory::default();
        h.push(100, 5, 1);
        let before = h.clone();
        h.push(101, 5, 1);
        assert_eq!(h, before, "an unchanged speed must not change the history");
        h.push(103, 9, 2);
        assert_eq!(h.series(104, 6), vec![(0, 0), (5, 1), (5, 1), (5, 1), (9, 2), (9, 2)]);
    }

    #[test]
    fn history_memory_is_fixed() {
        let mut h = SpeedHistory::default();
        for at in 0..10_000 {
            h.push(at, at % 7, at % 3);
        }
        assert!(h.samples.len() <= HISTORY_SECS as usize + 1);
        let s = h.series(9_999, HISTORY_SECS);
        assert_eq!(s.len(), HISTORY_SECS as usize);
        assert_eq!(s[HISTORY_SECS as usize - 1], (9_999 % 7, 9_999 % 3));
        // The window's first second still knows its speed.
        assert_eq!(s[0], ((9_999 - HISTORY_SECS + 1) % 7, (9_999 - HISTORY_SECS + 1) % 3));
    }

    #[test]
    fn split_handles_quotes() {
        assert_eq!(
            split_command(r#"/usr/bin/say "it is done" 'and \here' a\ b"#),
            vec!["/usr/bin/say", "it is done", r"and \here", "a b"]
        );
        assert_eq!(split_command(r#"  x ""  "#), vec!["x", ""]);
        assert!(split_command("   ").is_empty());
    }

    #[test]
    fn finish_command_adds_or_replaces() {
        assert_eq!(finish_command("notify", "Movie", "/d"), vec!["notify", "Movie", "/d"]);
        assert_eq!(
            finish_command("cp -r \"%F/%N\" /backup", "My file; rm -rf /", "/d"),
            vec!["cp", "-r", "/d/My file; rm -rf /", "/backup"]
        );
        assert!(finish_command("", "a", "b").is_empty());
    }

    #[test]
    fn squeeze_cells() {
        let have = [true, true, false, false, true, false];
        assert_eq!(squeeze(&have, 3), vec![255, 0, 127]);
        assert_eq!(squeeze(&have, 100), vec![255, 255, 0, 0, 255, 0]);
        assert!(squeeze(&[], 10).is_empty());
    }

    #[test]
    fn finish_only_after_downloading() {
        let mut t = Tracker::new(&AllTime::default());
        // Already complete when first seen: not a new finish.
        assert!(!t.check_finish("a", true, true));
        // Seen downloading, then finished: one finish.
        assert!(!t.check_finish("b", true, false));
        assert!(!t.check_finish("b", false, false));
        assert!(t.check_finish("b", true, true));
        assert!(!t.check_finish("b", true, true));
    }

    #[test]
    fn download_counter_restarts() {
        let mut t = Tracker::new(&AllTime::default());
        assert_eq!(t.count_download("a", Some(100)), 100);
        assert_eq!(t.count_download("a", Some(150)), 50);
        assert_eq!(t.count_download("a", Some(20)), 20);
        assert_eq!(t.count_download("a", None), 0);
        assert_eq!(t.count_download("a", Some(5)), 5);
    }

    fn view(state: State, done: u64, total: u64) -> TorrentView {
        TorrentView {
            key: "k".into(),
            name: "n".into(),
            state,
            error: None,
            total,
            done,
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
            trackers: Arc::default(),
            peers: Vec::new(),
            force_start: false,
            limits: Default::default(),
        }
    }

    #[test]
    fn quit_only_when_armed_and_nothing_left() {
        let mut t = Tracker::new(&AllTime::default());
        let seeding = [view(State::Seeding, 10, 10), view(State::Paused, 1, 10)];
        assert!(!t.should_quit(WhenAllDone::Quit, &seeding), "nothing finished yet");
        t.add_finished("k".into(), "n".into(), "/".into());
        assert!(t.should_quit(WhenAllDone::Quit, &seeding));
        assert!(!t.should_quit(WhenAllDone::Nothing, &seeding));
        let busy = [view(State::Seeding, 10, 10), view(State::Queued, 1, 10)];
        assert!(!t.should_quit(WhenAllDone::Quit, &busy));
        t.disarm();
        assert!(!t.should_quit(WhenAllDone::Quit, &seeding));
    }
}
