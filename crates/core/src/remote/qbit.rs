//! A part of qBittorrent's Web API, version 2, so that Sonarr, Radarr and the phone apps
//! made for qBittorrent can use RustTorrent.
//!
//! It answers on the same port as the page, behind the same Host and Origin checks (see
//! the [module](super) notes), with the same password. Any user name is taken. The
//! session cookie is `SID`, as qBittorrent's.
//!
//! What is left out, on purpose or for now:
//!
//! - Folders: `savepath` on add is ignored, and `setLocation` is refused, as the page
//!   cannot change folders either. They are changed at the computer.
//! - Preferences can be read, not changed.
//! - Tags given on add are ignored.
//! - Some numbers are not known to this engine: times a torrent finished or was last
//!   active, seeds and leechers apart (the connected peers are counted as seeds while
//!   downloading and as leechers while seeding), tracker status, and piece counts.
//! - `sync/maindata` always sends everything (`full_update`), never the changes since the
//!   last call.

use std::cmp::Ordering;
use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};

use super::{category_names, is_key, Check, Reply, Request, Server, MAX_CATEGORY, MAX_KEYS, QBIT_COOKIE};
use crate::intake::{self, Source};
use crate::manage::AddOptions;
use crate::model::{Snapshot, State, TorrentView};
use crate::priority::Priority;
use crate::rules::Move;
use crate::Command;

/// The qBittorrent version this API answers as. Sonarr and Radarr check the API version,
/// and 2.9.3 is the one qBittorrent 4.6 gives.
const APP_VERSION: &str = "v4.6.7";
const API_VERSION: &str = "2.9.3";
/// qBittorrent's "never" for a time left.
const FOREVER: u64 = 8_640_000;
/// More parts than this in one upload is not a real request.
const MAX_PARTS: usize = 64;
/// More links than this in one add is not a real request.
const MAX_LINKS: usize = 100;

/// The values of a request: from the address, and from a form in the body.
#[derive(Default, Debug)]
struct Params {
    values: Vec<(String, String)>,
    /// Files sent in a `multipart/form-data` body.
    files: Vec<Vec<u8>>,
}

impl Params {
    fn of(request: &Request) -> Self {
        let mut params = Self::default();
        params.form(&request.query);
        let kind = request.header("content-type").unwrap_or("");
        let main = kind.split(';').next().unwrap_or("").trim().to_ascii_lowercase();
        if main == "application/x-www-form-urlencoded" {
            params.form(&String::from_utf8_lossy(&request.body));
        } else if main == "multipart/form-data" {
            if let Some(boundary) = boundary(kind) {
                params.multipart(&request.body, &boundary);
            }
        }
        params
    }

    fn form(&mut self, text: &str) {
        let decode = |s: &str| intake::percent_decode(&s.replace('+', " "));
        for pair in text.split('&').filter(|p| !p.is_empty()) {
            let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
            self.values.push((decode(name), decode(value)));
        }
    }

    fn multipart(&mut self, body: &[u8], boundary: &str) {
        let delimiter = format!("--{boundary}");
        let mut pieces = split(body, delimiter.as_bytes()).into_iter().skip(1);
        for piece in pieces.by_ref().take(MAX_PARTS) {
            // The last delimiter is followed by `--`.
            if piece.starts_with(b"--") {
                break;
            }
            let piece = piece.strip_prefix(b"\r\n").unwrap_or(piece);
            let Some(end) = find(piece, b"\r\n\r\n") else {
                continue;
            };
            let head = String::from_utf8_lossy(&piece[..end]);
            let data = &piece[end + 4..];
            let data = data.strip_suffix(b"\r\n").unwrap_or(data);
            let (mut name, mut filename) = (None, None);
            for line in head.lines() {
                let Some((header, value)) = line.split_once(':') else {
                    continue;
                };
                if !header.trim().eq_ignore_ascii_case("content-disposition") {
                    continue;
                }
                for field in value.split(';').skip(1) {
                    if let Some((key, value)) = field.trim().split_once('=') {
                        let value = value.trim().trim_matches('"').to_owned();
                        match key.trim() {
                            "name" => name = Some(value),
                            "filename" => filename = Some(value),
                            _ => {}
                        }
                    }
                }
            }
            if filename.is_some() || name.as_deref() == Some("torrents") {
                self.files.push(data.to_vec());
            } else if let Some(name) = name {
                self.values.push((name, String::from_utf8_lossy(data).into_owned()));
            }
        }
    }

    fn get(&self, name: &str) -> Option<&str> {
        self.values.iter().find(|(n, _)| n == name).map(|(_, v)| v.as_str())
    }

    fn yes(&self, name: &str) -> bool {
        self.get(name).is_some_and(|v| v.eq_ignore_ascii_case("true"))
    }

    fn number(&self, name: &str) -> Option<f64> {
        self.get(name)?.trim().parse::<f64>().ok().filter(|n| n.is_finite())
    }
}

/// The boundary named in a `multipart/form-data` content type, quoted or not.
fn boundary(kind: &str) -> Option<String> {
    kind.split(';').find_map(|part| {
        let (name, value) = part.trim().split_once('=')?;
        let value = value.trim().trim_matches('"');
        (name.trim().eq_ignore_ascii_case("boundary") && !value.is_empty()).then(|| value.to_owned())
    })
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

fn split<'a>(mut body: &'a [u8], delimiter: &[u8]) -> Vec<&'a [u8]> {
    let mut out = Vec::new();
    while let Some(at) = find(body, delimiter) {
        out.push(&body[..at]);
        body = &body[at + delimiter.len()..];
    }
    out.push(body);
    out
}

/// Finished downloading what was chosen.
fn finished(t: &TorrentView) -> bool {
    matches!(t.state, State::Seeding | State::Completed) || (t.total > 0 && t.done >= t.total)
}

/// The state as qBittorrent names it.
fn state(t: &TorrentView) -> &'static str {
    let done = finished(t);
    let pick = |up: &'static str, down: &'static str| if done { up } else { down };
    match t.state {
        State::FetchingInfo => "metaDL",
        State::Checking => pick("checkingUP", "checkingDL"),
        State::Moving => "moving",
        State::Downloading if t.force_start => "forcedDL",
        State::Downloading if t.down_bps == 0 => "stalledDL",
        State::Downloading => "downloading",
        State::Seeding if t.force_start => "forcedUP",
        State::Seeding if t.up_bps == 0 => "stalledUP",
        State::Seeding => "uploading",
        State::Paused => pick("pausedUP", "pausedDL"),
        State::Queued | State::Scheduled => pick("queuedUP", "queuedDL"),
        State::Completed => "pausedUP",
        State::Blocked => pick("stalledUP", "stalledDL"),
        State::Error => "error",
    }
}

/// A filter of `torrents/info`.
fn passes(filter: &str, t: &TorrentView, state: &str) -> bool {
    match filter {
        "downloading" => state.ends_with("DL") || state == "downloading",
        "seeding" => matches!(
            state,
            "uploading" | "stalledUP" | "checkingUP" | "queuedUP" | "forcedUP"
        ),
        "completed" => finished(t),
        "paused" | "stopped" => state.starts_with("paused"),
        "resumed" | "running" => !state.starts_with("paused"),
        "active" => t.down_bps > 0 || t.up_bps > 0,
        "inactive" => t.down_bps == 0 && t.up_bps == 0,
        "stalled" => state.starts_with("stalled"),
        "stalled_uploading" => state == "stalledUP",
        "stalled_downloading" => state == "stalledDL",
        "checking" => state.starts_with("checking"),
        "moving" => state == "moving",
        "errored" => state == "error",
        _ => true,
    }
}

/// A speed limit in bytes a second, as qBittorrent gives a torrent's: -1 for none.
fn torrent_speed(kib: u32) -> i64 {
    if kib == 0 {
        -1
    } else {
        i64::from(kib) * 1024
    }
}

/// A limit in bytes a second to KiB a second. 0 or less is no limit. A limit under a KiB
/// is kept as one KiB, not turned into no limit.
fn to_kib(bytes: f64) -> u32 {
    if bytes <= 0.0 {
        0
    } else {
        (bytes / 1024.0).ceil().min(f64::from(u32::MAX)) as u32
    }
}

/// A torrent's own limit as qBittorrent gives it: -2 for the one in the settings, -1 for
/// none.
fn own_limit(own: Option<u32>, scale: f64) -> f64 {
    match own {
        None => -2.0,
        Some(0) => -1.0,
        Some(n) => f64::from(n) / scale,
    }
}

/// The limit that applies to a torrent: -1 for none.
fn applied_limit(own: Option<u32>, global: u32, scale: f64) -> f64 {
    match own.unwrap_or(global) {
        0 => -1.0,
        n => f64::from(n) / scale,
    }
}

fn ratio(t: &TorrentView) -> f64 {
    if t.done == 0 {
        0.0
    } else {
        t.uploaded as f64 / t.done as f64
    }
}

fn eta(t: &TorrentView) -> u64 {
    let left = t.total.saturating_sub(t.done);
    if left == 0 || t.down_bps == 0 {
        FOREVER
    } else {
        (left / t.down_bps).min(FOREVER)
    }
}

/// The folder the torrent is saved in, as qBittorrent reports it: for a torrent with
/// several files, the folder above the one of its own name that holds them. A torrent
/// added by an earlier version may have its files straight in its folder.
fn save_path(t: &TorrentView) -> String {
    let path = Path::new(&t.save_path);
    match (t.files.len(), path.file_name(), path.parent()) {
        (n, Some(last), Some(parent)) if n != 1 && last == t.own_name.as_str() => parent.to_string_lossy().into_owned(),
        _ => t.save_path.clone(),
    }
}

/// Where the torrent's data is: its one file, or the folder that holds its files.
fn content_path(t: &TorrentView) -> PathBuf {
    match t.files.as_slice() {
        [one] => Path::new(&t.save_path).join(&one.name),
        _ => PathBuf::from(&t.save_path),
    }
}

/// Connected peers as (seeds, leechers): see the module notes.
fn peers(t: &TorrentView) -> (u32, u32) {
    if finished(t) {
        (0, t.peers_live)
    } else {
        (t.peers_live, 0)
    }
}

/// One JSON object with the fields of both. (`json!` runs out of room past about 30.)
fn join(mut first: Value, rest: Value) -> Value {
    if let (Value::Object(a), Value::Object(b)) = (&mut first, rest) {
        a.extend(b);
    }
    first
}

fn torrent_json(t: &TorrentView, snap: &Snapshot) -> Value {
    let s = &snap.settings;
    let seeding = &t.limits.seeding;
    let (seeds, leechers) = peers(t);
    let first = json!({
        "added_on": t.added_at,
        "amount_left": t.total.saturating_sub(t.done),
        "auto_tmm": false,
        "availability": -1,
        "category": t.category.as_deref().unwrap_or(""),
        "completed": t.done,
        "completion_on": -1,
        "content_path": content_path(t).to_string_lossy(),
        "dl_limit": torrent_speed(t.down_limit_kib),
        "dlspeed": t.down_bps,
        "downloaded": t.done,
        "eta": eta(t),
        "f_l_piece_prio": false,
        "force_start": t.force_start,
        "hash": t.key,
        "infohash_v1": t.key,
        "infohash_v2": "",
        "magnet_uri": crate::manage::magnet_link(t),
        "max_ratio": applied_limit(t.ratio_limit_pct, s.ratio_limit_pct, 100.0),
        "max_seeding_time": applied_limit(seeding.time_limit_min, s.limits.seed_time_min, 1.0),
        "max_inactive_seeding_time": applied_limit(seeding.idle_limit_min, s.limits.seed_idle_min, 1.0),
    });
    let rest = json!({
        "name": t.name,
        "num_complete": t.peers_seen,
        "num_incomplete": 0,
        "num_leechs": leechers,
        "num_seeds": seeds,
        "priority": t.queue_pos.unwrap_or(0),
        "progress": t.progress(),
        "ratio": ratio(t),
        "ratio_limit": own_limit(t.ratio_limit_pct, 100.0),
        "save_path": save_path(t),
        "seeding_time": seeding.secs,
        "seeding_time_limit": own_limit(seeding.time_limit_min, 1.0),
        "inactive_seeding_time_limit": own_limit(seeding.idle_limit_min, 1.0),
        "seq_dl": false,
        "size": t.total,
        "state": state(t),
        "super_seeding": false,
        "tags": t.tags.join(", "),
        "total_size": t.files.iter().map(|f| f.len).sum::<u64>().max(t.total),
        "tracker": t.trackers.first().map(String::as_str).unwrap_or(""),
        "trackers_count": t.trackers.len(),
        "up_limit": torrent_speed(t.up_limit_kib),
        "uploaded": t.uploaded,
        "upspeed": t.up_bps,
    });
    join(first, rest)
}

fn properties_json(t: &TorrentView) -> Value {
    let (seeds, leechers) = peers(t);
    let first = json!({
        "addition_date": t.added_at,
        "comment": "",
        "completion_date": -1,
        "created_by": "",
        "creation_date": -1,
        "dl_limit": torrent_speed(t.down_limit_kib),
        "dl_speed": t.down_bps,
        "dl_speed_avg": t.down_bps,
        "eta": eta(t),
        "hash": t.key,
        "infohash_v1": t.key,
        "infohash_v2": "",
        "last_seen": -1,
        "name": t.name,
    });
    let rest = json!({
        "nb_connections": t.peers_live,
        "nb_connections_limit": -1,
        "peers": leechers,
        "peers_total": t.peers_seen,
        "piece_size": -1,
        "pieces_have": -1,
        "pieces_num": -1,
        "reannounce": 0,
        "save_path": save_path(t),
        "seeding_time": t.limits.seeding.secs,
        "seeds": seeds,
        "seeds_total": t.peers_seen,
        "share_ratio": ratio(t),
        "time_elapsed": t.limits.seeding.secs,
        "total_downloaded": t.done,
        "total_downloaded_session": t.done,
        "total_size": t.total,
        "total_uploaded": t.uploaded,
        "total_uploaded_session": t.uploaded,
        "total_wasted": 0,
        "up_limit": torrent_speed(t.up_limit_kib),
        "up_speed": t.up_bps,
        "up_speed_avg": t.up_bps,
    });
    join(first, rest)
}

/// qBittorrent's file priorities: 0 skips the file, 1 is normal and 6 high. It has no
/// low one, so low shows as normal.
fn file_priority(t: &TorrentView, i: usize) -> u8 {
    if t.selected.as_ref().is_some_and(|s| !s.contains(&i)) {
        return 0;
    }
    match t.priorities.get(i) {
        Priority::High => 6,
        Priority::Normal | Priority::Low => 1,
    }
}

fn files_json(t: &TorrentView) -> Value {
    let files: Vec<Value> = (t.files.iter().enumerate())
        .map(|(i, f)| {
            let done = t.file_done.get(i).copied().unwrap_or(0);
            let progress = if f.len == 0 { 1.0 } else { done as f64 / f.len as f64 };
            json!({
                "index": i,
                "name": f.name,
                "size": f.len,
                "progress": progress,
                "priority": file_priority(t, i),
                "is_seed": false,
                "piece_range": [0, 0],
                "availability": -1,
            })
        })
        .collect();
    Value::Array(files)
}

fn trackers_json(t: &TorrentView) -> Value {
    let trackers: Vec<Value> = (t.trackers.iter().enumerate())
        .map(|(tier, url)| {
            // 1 is "not contacted yet": this engine does not say how a tracker is doing.
            json!({
                "url": url, "status": 1, "tier": tier, "num_peers": -1, "num_seeds": -1,
                "num_leeches": -1, "num_downloaded": -1, "msg": "",
            })
        })
        .collect();
    Value::Array(trackers)
}

fn categories_json(snap: &Snapshot) -> Value {
    let map: Map<String, Value> = category_names(snap)
        .into_iter()
        .map(|name| {
            let dir = snap.settings.category_dirs.get(name);
            let path = dir.map(|d| d.to_string_lossy().into_owned()).unwrap_or_default();
            (name.clone(), json!({ "name": name, "savePath": path }))
        })
        .collect();
    Value::Object(map)
}

fn tag_names(snap: &Snapshot) -> Vec<&String> {
    let mut tags: Vec<&String> = snap.torrents.iter().flat_map(|t| &t.tags).collect();
    tags.sort();
    tags.dedup();
    tags
}

fn preferences_json(snap: &Snapshot) -> Value {
    let s = &snap.settings;
    let enabled = |n: u32| n > 0;
    let or_none = |n: u32| if n == 0 { -1 } else { i64::from(n) };
    json!({
        "save_path": s.download_dir.to_string_lossy(),
        "temp_path_enabled": s.incomplete_dir.is_some(),
        "temp_path": s.incomplete_dir.as_ref().map(|d| d.to_string_lossy().into_owned()).unwrap_or_default(),
        "max_ratio_enabled": enabled(s.ratio_limit_pct),
        "max_ratio": if s.ratio_limit_pct == 0 { -1.0 } else { f64::from(s.ratio_limit_pct) / 100.0 },
        "max_seeding_time_enabled": enabled(s.limits.seed_time_min),
        "max_seeding_time": or_none(s.limits.seed_time_min),
        "max_inactive_seeding_time_enabled": enabled(s.limits.seed_idle_min),
        "max_inactive_seeding_time": or_none(s.limits.seed_idle_min),
        // A torrent that reaches a limit stops; it is not removed.
        "max_ratio_act": 0,
        "queueing_enabled": enabled(s.max_downloads) || enabled(s.limits.max_seeding) || enabled(s.limits.max_active),
        "max_active_downloads": or_none(s.max_downloads),
        "max_active_uploads": or_none(s.limits.max_seeding),
        "max_active_torrents": or_none(s.limits.max_active),
        "dht": true,
        "dl_limit": u64::from(s.down_limit_kib) * 1024,
        "up_limit": u64::from(s.up_limit_kib) * 1024,
        "alt_dl_limit": u64::from(s.limits.slow.down_kib) * 1024,
        "alt_up_limit": u64::from(s.limits.slow.up_kib) * 1024,
    })
}

fn transfer_json(snap: &Snapshot) -> Value {
    json!({
        "dl_info_speed": snap.down_bps,
        "up_info_speed": snap.up_bps,
        "dl_info_data": snap.torrents.iter().map(|t| t.done).sum::<u64>(),
        "up_info_data": snap.torrents.iter().map(|t| t.uploaded).sum::<u64>(),
        "dl_rate_limit": u64::from(snap.settings.down_limit_kib) * 1024,
        "up_rate_limit": u64::from(snap.settings.up_limit_kib) * 1024,
        "dht_nodes": 0,
        "connection_status": if snap.blocked.is_some() { "disconnected" } else { "connected" },
    })
}

fn maindata_json(params: &Params, snap: &Snapshot) -> Value {
    let torrents: Map<String, Value> = (snap.torrents.iter())
        .filter(|t| !t.is_pending())
        .map(|t| (t.key.clone(), torrent_json(t, snap)))
        .collect();
    let mut state = transfer_json(snap);
    if let Value::Object(map) = &mut state {
        map.insert("use_alt_speed_limits".into(), json!(snap.slow.is_some()));
        map.insert("queueing".into(), json!(snap.settings.max_downloads > 0));
        map.insert("refresh_interval".into(), json!(1500));
        let free = crate::disk::free_bytes(&snap.settings.download_dir);
        map.insert("free_space_on_disk".into(), json!(free.unwrap_or(0)));
    }
    let rid = params.number("rid").unwrap_or(0.0).max(0.0) as u64;
    json!({
        "rid": rid + 1,
        "full_update": true,
        "torrents": torrents,
        "categories": categories_json(snap),
        "tags": tag_names(snap),
        "server_state": state,
    })
}

/// Orders by a field of the torrents' JSON: numbers by value, text without regard to case.
fn compare(a: &Value, b: &Value) -> Ordering {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => (x.as_f64().unwrap_or(0.0)).total_cmp(&y.as_f64().unwrap_or(0.0)),
        (Value::String(x), Value::String(y)) => x.to_lowercase().cmp(&y.to_lowercase()),
        (Value::Bool(x), Value::Bool(y)) => x.cmp(y),
        _ => Ordering::Equal,
    }
}

fn info_json(params: &Params, snap: &Snapshot) -> Value {
    let wanted = params.get("hashes").map(|h| {
        h.split('|')
            .map(|k| k.trim().to_ascii_lowercase())
            .take(MAX_KEYS)
            .collect::<Vec<_>>()
    });
    let mut list: Vec<Value> = (snap.torrents.iter())
        .filter(|t| !t.is_pending())
        .filter(|t| params.get("filter").is_none_or(|f| passes(f, t, state(t))))
        .filter(|t| {
            params
                .get("category")
                .is_none_or(|c| t.category.as_deref().unwrap_or("") == c)
        })
        .filter(|t| match params.get("tag") {
            None => true,
            Some("") => t.tags.is_empty(),
            Some(tag) => t.tags.iter().any(|x| x == tag),
        })
        .filter(|t| {
            wanted
                .as_ref()
                .is_none_or(|w| w.iter().any(|k| k.eq_ignore_ascii_case(&t.key)))
        })
        .map(|t| torrent_json(t, snap))
        .collect();
    if let Some(field) = params.get("sort") {
        list.sort_by(|a, b| compare(&a[field], &b[field]));
    }
    if params.yes("reverse") {
        list.reverse();
    }
    let offset = params.number("offset").unwrap_or(0.0) as i64;
    let start = if offset < 0 {
        list.len().saturating_sub(offset.unsigned_abs() as usize)
    } else {
        (offset as usize).min(list.len())
    };
    let limit = params.number("limit").filter(|n| *n > 0.0).map(|n| n as usize);
    let list: Vec<Value> = list.into_iter().skip(start).take(limit.unwrap_or(usize::MAX)).collect();
    Value::Array(list)
}

/// The torrents a request names in `hashes` (or `hash`), as their keys in the list.
/// `all` names every one.
fn keys(params: &Params, snap: &Snapshot) -> Vec<String> {
    let Some(hashes) = params.get("hashes").or_else(|| params.get("hash")) else {
        return Vec::new();
    };
    let known = snap.torrents.iter().filter(|t| !t.is_pending());
    if hashes.trim() == "all" {
        return known.map(|t| t.key.clone()).take(MAX_KEYS).collect();
    }
    let wanted: Vec<&str> = (hashes.split('|').map(str::trim))
        .filter(|h| is_key(h))
        .take(MAX_KEYS)
        .collect();
    known
        .filter(|t| wanted.iter().any(|w| w.eq_ignore_ascii_case(&t.key)))
        .map(|t| t.key.clone())
        .collect()
}

fn one<'a>(params: &Params, snap: &'a Snapshot) -> Option<&'a TorrentView> {
    let key = keys(params, snap).into_iter().next()?;
    snap.torrents.iter().find(|t| t.key == key)
}

/// A reply to a request that only reads, or `None` when it is not one.
fn read(name: &str, params: &Params, snap: &Snapshot) -> Option<Reply> {
    let not_found = || Reply::text(404, "Torrent hash was not found");
    Some(match name {
        "app/version" => Reply::text(200, APP_VERSION),
        "app/webapiVersion" => Reply::text(200, API_VERSION),
        "app/buildInfo" => Reply::json(
            200,
            json!({ "qt": "", "libtorrent": "", "boost": "", "openssl": "", "bitness": 64,
                    "app": format!("RustTorrent {}", crate::VERSION) }),
        ),
        "app/defaultSavePath" => Reply::text(200, &snap.settings.download_dir.to_string_lossy()),
        "app/preferences" => Reply::json(200, preferences_json(snap)),
        "torrents/info" => Reply::json(200, info_json(params, snap)),
        "torrents/properties" => one(params, snap).map_or_else(not_found, |t| Reply::json(200, properties_json(t))),
        "torrents/files" => one(params, snap).map_or_else(not_found, |t| Reply::json(200, files_json(t))),
        "torrents/trackers" => one(params, snap).map_or_else(not_found, |t| Reply::json(200, trackers_json(t))),
        "torrents/categories" => Reply::json(200, categories_json(snap)),
        "torrents/tags" => Reply::json(200, json!(tag_names(snap))),
        "sync/maindata" => Reply::json(200, maindata_json(params, snap)),
        "transfer/info" => Reply::json(200, transfer_json(snap)),
        "transfer/speedLimitsMode" => Reply::text(200, if snap.slow.is_some() { "1" } else { "0" }),
        "transfer/downloadLimit" => Reply::text(200, &(u64::from(snap.settings.down_limit_kib) * 1024).to_string()),
        "transfer/uploadLimit" => Reply::text(200, &(u64::from(snap.settings.up_limit_kib) * 1024).to_string()),
        _ => return None,
    })
}

type Answer = Result<(Vec<Command>, &'static str), (u16, &'static str)>;

fn add(params: &Params) -> Answer {
    let category = params.get("category").filter(|c| !c.is_empty()).map(str::to_owned);
    if category.as_ref().is_some_and(|c| c.chars().count() > MAX_CATEGORY) {
        return Err((400, "That category name is too long."));
    }
    let paused = params.yes("paused") || params.yes("stopped");
    let mut commands = Vec::new();
    let links = params.get("urls").unwrap_or("").lines().map(str::trim);
    for text in links.filter(|l| !l.is_empty()).take(MAX_LINKS) {
        // A path would name a file on the computer, which is not the API's to read.
        if !matches!(intake::parse(text), Ok(Source::Link(_))) {
            continue;
        }
        commands.push(if category.is_none() && !paused {
            Command::Add(text.to_owned())
        } else {
            Command::AddWith(AddOptions {
                text: text.to_owned(),
                category: category.clone(),
                paused,
                ..Default::default()
            })
        });
    }
    for bytes in &params.files {
        if librqbit::torrent_from_bytes(bytes).is_ok() {
            commands.push(Command::AddTorrentBytes {
                bytes: bytes.clone(),
                category: category.clone(),
                paused,
            });
        }
    }
    if commands.is_empty() {
        return Err((415, "Fails."));
    }
    Ok((commands, "Ok."))
}

/// qBittorrent's share limits: -2 follows the settings, -1 is no limit.
fn share_limit(value: Option<f64>, scale: f64) -> Option<Option<u32>> {
    let value = value?;
    Some(if value <= -1.5 {
        None
    } else if value < 0.0 {
        Some(0)
    } else {
        // 0 is "no limit" here, so the smallest limit is kept as 1.
        Some(((value * scale).round().min(f64::from(u32::MAX)) as u32).max(1))
    })
}

/// The commands a request that changes something asks for, or `None` when it is not one.
fn act(name: &str, params: &Params, snap: &Snapshot) -> Option<Answer> {
    let keys = || keys(params, snap);
    let list = |text: Option<&str>, by: char| -> Vec<String> {
        (text.unwrap_or("").split(by).map(str::trim))
            .filter(|s| !s.is_empty())
            .take(MAX_KEYS)
            .map(str::to_owned)
            .collect()
    };
    let queue = |how| Ok((vec![Command::MoveInQueue { keys: keys(), how }], ""));
    let settings = &snap.settings;
    Some(match name {
        "torrents/add" => add(params),
        "torrents/pause" | "torrents/stop" => Ok((vec![Command::Pause(keys())], "")),
        "torrents/resume" | "torrents/start" => Ok((vec![Command::Resume(keys())], "")),
        "torrents/delete" => Ok((
            vec![Command::Remove {
                keys: keys(),
                delete_files: params.yes("deleteFiles"),
            }],
            "",
        )),
        "torrents/recheck" => Ok((vec![Command::Recheck(keys())], "")),
        "torrents/reannounce" => Ok((vec![Command::Reannounce(keys())], "")),
        "torrents/topPrio" => queue(Move::Top),
        "torrents/bottomPrio" => queue(Move::Bottom),
        "torrents/increasePrio" => queue(Move::Up),
        "torrents/decreasePrio" => queue(Move::Down),
        "torrents/setForceStart" => Ok((
            vec![Command::SetForceStart {
                keys: keys(),
                on: params.yes("value"),
            }],
            "",
        )),
        "torrents/setCategory" => {
            let category = params.get("category").unwrap_or("").trim();
            if category.chars().count() > MAX_CATEGORY {
                return Some(Err((400, "That category name is too long.")));
            }
            let category = (!category.is_empty()).then(|| category.to_owned());
            Ok((vec![Command::SetCategory { keys: keys(), category }], ""))
        }
        "torrents/createCategory" => {
            let category = params.get("category").unwrap_or("").trim();
            if category.is_empty() || category.chars().count() > MAX_CATEGORY {
                return Some(Err((400, "Give a category name of up to 100 characters.")));
            }
            Ok((vec![Command::AddCategory(category.to_owned())], ""))
        }
        // The folder of a category is changed at the computer.
        "torrents/editCategory" => Ok((Vec::new(), "")),
        "torrents/removeCategories" => {
            let names = list(params.get("categories"), '\n');
            Ok((names.into_iter().map(Command::RemoveCategory).collect(), ""))
        }
        "torrents/addTags" | "torrents/removeTags" => {
            let on = name == "torrents/addTags";
            let keys = keys();
            let mut tags = list(params.get("tags"), ',');
            if !on && tags.is_empty() {
                // No tags named takes all of them off.
                let chosen = snap.torrents.iter().filter(|t| keys.contains(&t.key));
                tags = chosen.flat_map(|t| t.tags.iter().cloned()).collect();
                tags.sort();
                tags.dedup();
            }
            let commands = tags.into_iter().map(|tag| Command::SetTag {
                keys: keys.clone(),
                tag,
                on,
            });
            Ok((commands.collect(), ""))
        }
        // A tag exists here while a torrent has it.
        "torrents/createTags" => Ok((Vec::new(), "")),
        "torrents/deleteTags" => {
            let commands = list(params.get("tags"), ',').into_iter().map(|tag| {
                let keys = (snap.torrents.iter())
                    .filter(|t| t.tags.contains(&tag))
                    .map(|t| t.key.clone())
                    .collect();
                Command::SetTag { keys, tag, on: false }
            });
            Ok((commands.collect(), ""))
        }
        "torrents/setShareLimits" => {
            let keys = keys();
            let mut commands = Vec::new();
            if let Some(limit) = share_limit(params.number("ratioLimit"), 100.0) {
                commands.push(Command::SetRatioLimit {
                    keys: keys.clone(),
                    limit,
                });
            }
            let time = share_limit(params.number("seedingTimeLimit"), 1.0);
            let idle = share_limit(params.number("inactiveSeedingTimeLimit"), 1.0);
            if time.is_some() || idle.is_some() {
                // Each torrent keeps the limit the request leaves out.
                for t in snap.torrents.iter().filter(|t| keys.contains(&t.key)) {
                    let seeding = &t.limits.seeding;
                    commands.push(Command::SetSeedingLimits {
                        keys: vec![t.key.clone()],
                        time_limit_min: time.unwrap_or(seeding.time_limit_min),
                        idle_limit_min: idle.unwrap_or(seeding.idle_limit_min),
                    });
                }
            }
            Ok((commands, ""))
        }
        "torrents/setDownloadLimit" | "torrents/setUploadLimit" => {
            let Some(limit) = params.number("limit") else {
                return Some(Err((400, "Give a limit.")));
            };
            let kib = to_kib(limit);
            let down = name == "torrents/setDownloadLimit";
            let keys = keys();
            // Each torrent keeps its other limit.
            let commands =
                (snap.torrents.iter().filter(|t| keys.contains(&t.key))).map(|t| Command::SetTorrentLimits {
                    keys: vec![t.key.clone()],
                    down_kib: if down { kib } else { t.down_limit_kib },
                    up_kib: if down { t.up_limit_kib } else { kib },
                });
            Ok((commands.collect(), ""))
        }
        "torrents/filePrio" => {
            let Some(t) = one(params, snap) else {
                return Some(Err((404, "Torrent hash was not found")));
            };
            let ids: Vec<usize> = (params.get("id").unwrap_or("").split('|'))
                .filter_map(|i| i.trim().parse().ok())
                .filter(|&i| i < t.files.len())
                .collect();
            let Some(priority) = params.number("priority") else {
                return Some(Err((400, "Give a priority.")));
            };
            if ids.is_empty() {
                return Some(Err((400, "Give the files by their index.")));
            }
            let mut chosen: Vec<usize> = match &t.selected {
                Some(s) => s.clone(),
                None => (0..t.files.len()).collect(),
            };
            if priority == 0.0 {
                chosen.retain(|i| !ids.contains(i));
                if chosen.is_empty() {
                    return Some(Err((409, "Choose at least one file.")));
                }
                return Some(Ok((
                    vec![Command::SetFiles {
                        key: t.key.clone(),
                        files: chosen,
                    }],
                    "",
                )));
            }
            chosen.extend(ids.iter().copied());
            chosen.sort_unstable();
            chosen.dedup();
            let level = if priority >= 6.0 {
                Priority::High
            } else {
                Priority::Normal
            };
            Ok((
                vec![
                    Command::SetFiles {
                        key: t.key.clone(),
                        files: chosen,
                    },
                    Command::SetFilePriority {
                        key: t.key.clone(),
                        files: ids,
                        priority: level,
                    },
                ],
                "",
            ))
        }
        "torrents/rename" => {
            let Some(t) = one(params, snap) else {
                return Some(Err((404, "Torrent hash was not found")));
            };
            let name = params.get("name").unwrap_or("").to_owned();
            Ok((
                vec![Command::Rename {
                    key: t.key.clone(),
                    name,
                }],
                "",
            ))
        }
        "torrents/setLocation" => Err((403, "Folders can only be changed at the computer.")),
        "transfer/setDownloadLimit" | "transfer/setUploadLimit" => {
            let Some(limit) = params.number("limit") else {
                return Some(Err((400, "Give a limit.")));
            };
            let kib = to_kib(limit);
            let (down_kib, up_kib) = if name == "transfer/setDownloadLimit" {
                (kib, settings.up_limit_kib)
            } else {
                (settings.down_limit_kib, kib)
            };
            Ok((vec![Command::SetLimits { down_kib, up_kib }], ""))
        }
        "transfer/toggleSpeedLimitsMode" => Ok((vec![Command::SetSlowMode(snap.slow.is_none())], "")),
        "transfer/setSpeedLimitsMode" => Ok((vec![Command::SetSlowMode(params.get("mode") == Some("1"))], "")),
        _ => return None,
    })
}

/// Commands that name no torrent do nothing, so they are not sent.
fn empty(command: &Command) -> bool {
    match command {
        Command::Pause(keys) | Command::Resume(keys) | Command::Recheck(keys) | Command::Reannounce(keys) => {
            keys.is_empty()
        }
        Command::Remove { keys, .. }
        | Command::MoveInQueue { keys, .. }
        | Command::SetForceStart { keys, .. }
        | Command::SetCategory { keys, .. }
        | Command::SetTag { keys, .. }
        | Command::SetRatioLimit { keys, .. }
        | Command::SetSeedingLimits { keys, .. }
        | Command::SetTorrentLimits { keys, .. } => keys.is_empty(),
        _ => false,
    }
}

impl Server {
    pub(super) async fn qbit(&self, request: &Request) -> Reply {
        let name = &request.path["/api/v2/".len()..];
        let params = Params::of(request);
        match name {
            "auth/login" => return self.qbit_login(request, &params).await,
            "auth/logout" => {
                self.end_session(request);
                return Reply {
                    cookie: Some(String::new()),
                    cookie_name: QBIT_COOKIE,
                    ..Reply::text(200, "")
                };
            }
            _ => {}
        }
        if !self.signed_in(request) {
            return Reply::text(403, "Forbidden");
        }
        let snap = self.shared.lock().unwrap_or_else(|e| e.into_inner()).clone();
        if let Some(reply) = read(name, &params, &snap) {
            return reply;
        }
        let Some(answer) = act(name, &params, &snap) else {
            return Reply::text(404, "Not Found");
        };
        // Anything that changes something has to be a POST, which the gate has checked.
        if request.method != "POST" {
            return Reply::text(405, "Method Not Allowed");
        }
        match answer {
            Ok((commands, text)) => {
                for command in commands.into_iter().filter(|c| !empty(c)) {
                    let _ = self.tx.send(command);
                }
                Reply::text(200, text)
            }
            Err((status, text)) => Reply::text(status, text),
        }
    }

    async fn qbit_login(&self, request: &Request, params: &Params) -> Reply {
        if request.method != "POST" {
            return Reply::text(405, "Method Not Allowed");
        }
        let password = params.get("password").unwrap_or("").to_owned();
        match self.check_password(password).await {
            Check::Right => Reply {
                cookie: Some(self.new_session()),
                cookie_name: QBIT_COOKIE,
                ..Reply::text(200, "Ok.")
            },
            Check::Wrong => Reply::text(200, "Fails."),
            Check::Wait => Reply::text(403, "Too many tries. Wait a little and try again."),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(query: &str, kind: &str, body: &[u8]) -> Request {
        Request {
            method: "POST".into(),
            path: "/api/v2/torrents/add".into(),
            query: query.into(),
            headers: vec![("content-type".into(), kind.into())],
            body: body.to_vec(),
        }
    }

    #[test]
    fn forms() {
        let p = Params::of(&request(
            "hashes=a%7Cb&x",
            "application/x-www-form-urlencoded",
            b"category=tv+shows&paused=true",
        ));
        assert_eq!(p.get("hashes"), Some("a|b"));
        assert_eq!(p.get("x"), Some(""));
        assert_eq!(p.get("category"), Some("tv shows"));
        assert!(p.yes("paused") && !p.yes("stopped"));

        let body = b"--XyZ\r\nContent-Disposition: form-data; name=\"category\"\r\n\r\nmovies\r\n\
--XyZ\r\nContent-Disposition: form-data; name=\"torrents\"; filename=\"a.torrent\"\r\n\
Content-Type: application/x-bittorrent\r\n\r\nd1:a\r\n1:be\r\n--XyZ--\r\n";
        let p = Params::of(&request("", "multipart/form-data; boundary=\"XyZ\"", body));
        assert_eq!(p.get("category"), Some("movies"));
        assert_eq!(p.files, vec![b"d1:a\r\n1:be".to_vec()]);
    }

    #[test]
    fn share_limits() {
        assert_eq!(share_limit(None, 100.0), None);
        assert_eq!(share_limit(Some(-2.0), 100.0), Some(None));
        assert_eq!(share_limit(Some(-1.0), 100.0), Some(Some(0)));
        assert_eq!(share_limit(Some(1.5), 100.0), Some(Some(150)));
        assert_eq!(share_limit(Some(0.0), 1.0), Some(Some(1)));
        assert_eq!(to_kib(0.0), 0);
        assert_eq!(to_kib(-1.0), 0);
        assert_eq!(to_kib(100.0), 1);
        assert_eq!(to_kib(2048.0), 2);
    }

    #[test]
    fn states() {
        let mut t = TorrentView::sample("k", State::Downloading, 0, 10);
        assert_eq!(state(&t), "stalledDL");
        t.down_bps = 5;
        assert_eq!(state(&t), "downloading");
        t.force_start = true;
        assert_eq!(state(&t), "forcedDL");
        t.state = State::Paused;
        assert_eq!(state(&t), "pausedDL");
        t.done = 10;
        assert_eq!(state(&t), "pausedUP");
        t.state = State::Completed;
        assert_eq!(state(&t), "pausedUP");
        t.state = State::Queued;
        assert_eq!(state(&t), "queuedUP");
        t.state = State::FetchingInfo;
        assert_eq!(state(&t), "metaDL");
        assert!(passes("completed", &t, "pausedUP") && !passes("seeding", &t, "pausedUP"));
    }

    #[test]
    fn paths() {
        use crate::model::FileMeta;
        let file = |name: &str| FileMeta {
            name: name.into(),
            len: 1,
        };
        let mut t = TorrentView::sample("k", State::Downloading, 0, 10);
        t.name = "My show".into();
        t.own_name = "Show".into();
        // Several files in a folder of the torrent's name: saved in the folder above.
        t.save_path = "/d/Show".into();
        t.files = std::sync::Arc::new(vec![file("a.mkv"), file("b.mkv")]);
        assert_eq!(save_path(&t), "/d");
        assert_eq!(content_path(&t), PathBuf::from("/d/Show"));
        // Several files straight in the folder, as an earlier version saved them.
        t.save_path = "/d".into();
        assert_eq!(save_path(&t), "/d");
        assert_eq!(content_path(&t), PathBuf::from("/d"));
        // One file.
        t.own_name = "a.mkv".into();
        t.files = std::sync::Arc::new(vec![file("a.mkv")]);
        assert_eq!(save_path(&t), "/d");
        assert_eq!(content_path(&t), PathBuf::from("/d/a.mkv"));
    }
}
