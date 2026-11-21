//! Remote control: a small web server that shows the torrent list in a browser and takes
//! a few commands from it.
//!
//! A web server on the user's own machine can be reached by any web page open in their
//! browser, so every request has to pass these checks:
//!
//! - The `Host` header must be `localhost`, an IP address or a `.local` name. A page
//!   that points its own domain name at this machine fails here.
//! - Anything that changes something is a `POST` that carries the header
//!   `X-RustTorrent: 1` and, when the browser sends an `Origin`, an origin equal to the
//!   host. A page from another site can do neither without permission this server never
//!   gives.
//! - Everything but the page itself needs a session, which is given for the password and
//!   kept in a cookie that browsers do not send along with requests from other sites.
//!
//! The server speaks HTTP, one request per connection: plain, or inside TLS with a
//! certificate the app makes for itself (see [`tls`]) when the user turns HTTPS on.
//!
//! A `.torrent` file can be sent from the page. Only a signed-in request from the page
//! itself may send a body larger than a command, the body is read into memory only, up
//! to [`MAX_UPLOAD`], and it is taken only if it reads as a torrent.
//!
//! Under `/api/v2/` the same server speaks a part of qBittorrent's Web API, for phone apps
//! and for Sonarr and Radarr; see [`qbit`]. Those programs cannot send the page's own
//! header, so there the session cookie (`SID`, never sent along from other sites) and the
//! origin check have to do without it.
mod qbit;
pub(crate) mod tls;

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::num::NonZeroU32;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ring::pbkdf2;
use ring::rand::{SecureRandom, SystemRandom};
use serde::Deserialize;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::mpsc::UnboundedSender;
use tokio_rustls::TlsAcceptor;

use crate::intake::{self, Source};
use crate::limits::SlowReason;
use crate::model::Snapshot;
use crate::store::Settings;
use crate::{Command, Shared};

/// The shortest password the app accepts.
pub const MIN_PASSWORD: usize = 8;

const ROUNDS: NonZeroU32 = NonZeroU32::new(600_000).unwrap();
const MAX_HEAD: usize = 8 * 1024;
const MAX_BODY: usize = 16 * 1024;
const MAX_CONNECTIONS: usize = 32;
const TIME_LIMIT: Duration = Duration::from_secs(10);
const SESSION_LIFE: Duration = Duration::from_secs(7 * 24 * 3600);
const MAX_SESSIONS: usize = 16;
const MAX_KEYS: usize = 1000;
const COOKIE: &str = "rusttorrent";
/// The cookie qBittorrent's API uses for its session.
const QBIT_COOKIE: &str = "SID";
/// The largest `.torrent` file the page may send. Most are well under 1 MiB.
pub(crate) const MAX_UPLOAD: usize = 4 * 1024 * 1024;
/// More files than this in one choice is not a real request.
const MAX_FILES: usize = 100_000;
const MAX_CATEGORY: usize = 100;

const PAGE: &str = include_str!("index.html");
const SCRIPT: &str = include_str!("app.js");
const STYLE: &str = include_str!("app.css");

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub(crate) fn unhex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) || !text.is_ascii() {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).ok())
        .collect()
}

/// A long random text, hard to guess, for links that must stay private.
pub(crate) fn new_token() -> String {
    hex(&random(32))
}

fn random(len: usize) -> Vec<u8> {
    let mut out = vec![0u8; len];
    SystemRandom::new().fill(&mut out).expect("the system's random numbers");
    out
}

/// A new salt and the password's hash, both in hex, for the settings file.
pub(crate) fn hash_password(password: &str) -> (String, String) {
    let salt = random(16);
    let mut hash = [0u8; 32];
    pbkdf2::derive(
        pbkdf2::PBKDF2_HMAC_SHA256,
        ROUNDS,
        &salt,
        password.as_bytes(),
        &mut hash,
    );
    (hex(&salt), hex(&hash))
}

fn password_matches(salt: &[u8], hash: &[u8], password: &str) -> bool {
    pbkdf2::verify(pbkdf2::PBKDF2_HMAC_SHA256, ROUNDS, salt, password.as_bytes(), hash).is_ok()
}

/// Opens the port: on this machine alone, or for the whole local network.
pub(crate) async fn listen(lan: bool, port: u16) -> std::io::Result<TcpListener> {
    let ip = if lan {
        Ipv4Addr::UNSPECIFIED
    } else {
        Ipv4Addr::LOCALHOST
    };
    TcpListener::bind(SocketAddr::new(IpAddr::V4(ip), port)).await
}

struct Guard {
    failures: u32,
    locked_until: Option<Instant>,
}

enum Check {
    Right,
    Wrong,
    /// Too soon after the last try, or locked after too many wrong ones.
    Wait,
}

struct Server {
    salt: Vec<u8>,
    hash: Vec<u8>,
    shared: Shared,
    tx: UnboundedSender<Command>,
    /// Session tokens and when each stops being valid.
    sessions: Mutex<HashMap<String, Instant>>,
    guard: Mutex<Guard>,
    /// Requests arrive inside TLS.
    https: bool,
}

/// Answers requests until the task is dropped. Sessions live only as long as it does, so
/// changing the password signs everybody out.
pub(crate) async fn serve(
    listener: TcpListener,
    salt: Vec<u8>,
    hash: Vec<u8>,
    shared: Shared,
    tx: UnboundedSender<Command>,
    tls: Option<TlsAcceptor>,
) {
    let server = Arc::new(Server {
        salt,
        hash,
        shared,
        tx,
        sessions: Mutex::new(HashMap::new()),
        guard: Mutex::new(Guard {
            failures: 0,
            locked_until: None,
        }),
        https: tls.is_some(),
    });
    let mut connections = tokio::task::JoinSet::new();
    loop {
        while connections.try_join_next().is_some() {}
        let Ok((stream, _)) = listener.accept().await else {
            tokio::time::sleep(Duration::from_millis(100)).await;
            continue;
        };
        if connections.len() >= MAX_CONNECTIONS {
            continue;
        }
        let server = server.clone();
        let tls = tls.clone();
        connections.spawn(async move {
            let _ = tokio::time::timeout(TIME_LIMIT, async {
                match tls {
                    Some(tls) => match tls.accept(stream).await {
                        Ok(stream) => connection(&server, stream).await,
                        Err(_) => Ok(()),
                    },
                    None => connection(&server, stream).await,
                }
            })
            .await;
        });
    }
}

async fn connection<S: AsyncRead + AsyncWrite + Unpin>(server: &Server, mut stream: S) -> std::io::Result<()> {
    let reply = match read_request(server, &mut stream).await {
        Ok(request) => server.answer(&request).await,
        Err(status) => Reply::error(status, "The request could not be read."),
    };
    stream.write_all(&reply.into_bytes(server.https)).await?;
    stream.shutdown().await
}

#[derive(Debug, PartialEq, Eq)]
struct Request {
    method: String,
    /// Without the part after `?`.
    path: String,
    /// The part after `?`, still encoded.
    query: String,
    /// Names in lower case.
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Request {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(n, _)| n == name).map(|(_, v)| v.as_str())
    }

    /// The value of the session cookie this request's path uses.
    fn cookie(&self) -> Option<&str> {
        let wanted = if is_qbit(&self.path) { QBIT_COOKIE } else { COOKIE };
        self.header("cookie")?
            .split(';')
            .filter_map(|part| part.trim().split_once('='))
            .find(|(name, _)| *name == wanted)
            .map(|(_, value)| value)
    }
}

/// Reads the part of a request before its body. `Err` is the HTTP status to answer with.
fn parse_head(head: &str) -> Result<(Request, usize), u16> {
    let mut lines = head.split("\r\n");
    let mut first = lines.next().ok_or(400u16)?.split(' ');
    let (Some(method), Some(target), Some(version), None) = (first.next(), first.next(), first.next(), first.next())
    else {
        return Err(400);
    };
    if !version.starts_with("HTTP/1.") || !target.starts_with('/') {
        return Err(400);
    }
    let mut headers = Vec::new();
    for line in lines.filter(|l| !l.is_empty()) {
        let (name, value) = line.split_once(':').ok_or(400u16)?;
        headers.push((name.trim().to_ascii_lowercase(), value.trim().to_owned()));
    }
    // Two lengths, two hosts or a body sent in pieces are how one request is made to
    // look like another. None of them is needed here.
    let count = |name: &str| headers.iter().filter(|(n, _)| n == name).count();
    if count("content-length") > 1 || count("host") > 1 || count("transfer-encoding") > 0 {
        return Err(400);
    }
    let length = match headers.iter().find(|(n, _)| n == "content-length") {
        Some((_, v)) => v.parse::<usize>().map_err(|_| 400u16)?,
        None => 0,
    };
    // Anything over a command's size is only read for an upload; see `read_request`.
    if length > MAX_UPLOAD {
        return Err(413);
    }
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let request = Request {
        method: method.to_owned(),
        path: path.to_owned(),
        query: query.to_owned(),
        headers,
        body: Vec::new(),
    };
    Ok((request, length))
}

async fn read_request<S: AsyncRead + AsyncWrite + Unpin>(server: &Server, stream: &mut S) -> Result<Request, u16> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 2048];
    let end = loop {
        if let Some(at) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break at;
        }
        if buf.len() > MAX_HEAD {
            return Err(431);
        }
        match stream.read(&mut chunk).await {
            Ok(n) if n > 0 => buf.extend_from_slice(&chunk[..n]),
            _ => return Err(400),
        }
    };
    let head = std::str::from_utf8(&buf[..end]).map_err(|_| 400u16)?;
    let (mut request, length) = parse_head(head)?;
    if length > MAX_BODY {
        // Checked before a byte of it is read, so nobody else can make the app hold
        // megabytes in memory.
        let upload = request.path == "/api/upload" || request.path == "/api/v2/torrents/add";
        if !(request.method == "POST" && upload) || server.gate(&request).is_err() || !server.signed_in(&request) {
            return Err(413);
        }
        if request
            .header("expect")
            .is_some_and(|e| e.eq_ignore_ascii_case("100-continue"))
        {
            stream
                .write_all(b"HTTP/1.1 100 Continue\r\n\r\n")
                .await
                .map_err(|_| 400u16)?;
        }
    }
    let mut body = buf[end + 4..].to_vec();
    while body.len() < length {
        match stream.read(&mut chunk).await {
            Ok(n) if n > 0 => body.extend_from_slice(&chunk[..n]),
            _ => return Err(400),
        }
    }
    body.truncate(length);
    request.body = body;
    Ok(request)
}

/// The host without its port.
fn host_name(host: &str) -> &str {
    if let Some(rest) = host.strip_prefix('[') {
        return rest.split(']').next().unwrap_or("");
    }
    match host.rsplit_once(':') {
        Some((name, port)) if !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()) => name,
        _ => host,
    }
}

/// Paths of the qBittorrent-style API.
fn is_qbit(path: &str) -> bool {
    path.starts_with("/api/v2/")
}

/// True for the names this server can honestly be reached by. A domain name from the
/// internet is never one of them.
fn host_allowed(host: &str) -> bool {
    let name = host_name(host).to_ascii_lowercase();
    name == "localhost" || name.ends_with(".local") || name.parse::<IpAddr>().is_ok()
}

struct Reply {
    status: u16,
    kind: &'static str,
    body: Vec<u8>,
    /// A new value for the session cookie. An empty one removes the cookie.
    cookie: Option<String>,
    /// The session cookie's name.
    cookie_name: &'static str,
}

impl Reply {
    fn file(kind: &'static str, body: &str) -> Self {
        Self {
            status: 200,
            kind,
            body: body.as_bytes().to_vec(),
            cookie: None,
            cookie_name: COOKIE,
        }
    }

    /// Plain text, as qBittorrent's API answers most requests.
    fn text(status: u16, body: &str) -> Self {
        Self {
            status,
            ..Self::file("text/plain; charset=utf-8", body)
        }
    }

    fn json(status: u16, value: serde_json::Value) -> Self {
        Self {
            status,
            kind: "application/json",
            body: value.to_string().into_bytes(),
            cookie: None,
            cookie_name: COOKIE,
        }
    }

    fn ok() -> Self {
        Self::json(200, serde_json::json!({ "ok": true }))
    }

    fn error(status: u16, text: &str) -> Self {
        Self::json(status, serde_json::json!({ "error": text }))
    }

    /// `secure` marks the cookie as one for HTTPS only.
    fn into_bytes(self, secure: bool) -> Vec<u8> {
        let reason = match self.status {
            200 => "OK",
            400 => "Bad Request",
            401 => "Unauthorized",
            403 => "Forbidden",
            404 => "Not Found",
            409 => "Conflict",
            413 => "Content Too Large",
            415 => "Unsupported Media Type",
            429 => "Too Many Requests",
            405 => "Method Not Allowed",
            431 => "Request Header Fields Too Large",
            _ => "Error",
        };
        let mut head = format!(
            "HTTP/1.1 {} {reason}\r\n\
             Content-Type: {}\r\n\
             Content-Length: {}\r\n\
             Connection: close\r\n\
             Cache-Control: no-store\r\n\
             X-Content-Type-Options: nosniff\r\n\
             X-Frame-Options: DENY\r\n\
             Referrer-Policy: no-referrer\r\n\
             Content-Security-Policy: default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; form-action 'none'; base-uri 'none'\r\n",
            self.status,
            self.kind,
            self.body.len()
        );
        if let Some(token) = &self.cookie {
            let age = if token.is_empty() { 0 } else { SESSION_LIFE.as_secs() };
            let secure = if secure { "; Secure" } else { "" };
            head.push_str(&format!(
                "Set-Cookie: {}={token}; Path=/; Max-Age={age}; HttpOnly; SameSite=Strict{secure}\r\n",
                self.cookie_name
            ));
        }
        head.push_str("\r\n");
        let mut out = head.into_bytes();
        out.extend_from_slice(&self.body);
        out
    }
}

/// What the page may ask for. Folders, the network interface and the remote control's own
/// settings are left out on purpose: they can only be changed at the computer.
#[derive(Deserialize, Debug, PartialEq, Eq)]
#[serde(tag = "do", rename_all = "snake_case")]
enum Ask {
    Add {
        text: String,
    },
    Pause {
        keys: Vec<String>,
    },
    Resume {
        keys: Vec<String>,
    },
    Remove {
        keys: Vec<String>,
        #[serde(default)]
        delete_files: bool,
    },
    Limits {
        down_kib: u32,
        up_kib: u32,
    },
    /// Which files of one torrent to download, by their place in its list.
    Files {
        key: String,
        files: Vec<usize>,
    },
    Category {
        keys: Vec<String>,
        category: Option<String>,
    },
    /// Slow mode on or off by hand.
    SlowMode {
        on: bool,
    },
    /// Slow mode's speeds, seeding limits and queue limits. A field left out keeps its
    /// value. Slow mode's schedule and the transfer schedule are left to the computer.
    Rules(RulesAsk),
}

/// The numbers the page may change. 0 means no limit, as in the app.
#[derive(Deserialize, Debug, Default, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct RulesAsk {
    slow_down_kib: Option<u32>,
    slow_up_kib: Option<u32>,
    ratio_limit_pct: Option<u32>,
    seed_time_min: Option<u32>,
    seed_idle_min: Option<u32>,
    max_downloads: Option<u32>,
    max_seeding: Option<u32>,
    max_active: Option<u32>,
}

/// More than this in any of the rules is a typing mistake, not a wish.
const MAX_RULE: u32 = 10_000_000;

impl Ask {
    /// The commands that carry out the ask. `settings` are the current ones, for the
    /// values an ask leaves as they are.
    fn into_commands(self, settings: &Settings) -> Result<Vec<Command>, &'static str> {
        let checked = |keys: Vec<String>| {
            if keys.len() > MAX_KEYS {
                Err("Too many torrents at once.")
            } else {
                Ok(keys)
            }
        };
        Ok(vec![match self {
            // A path would name a file on the computer, which is not the remote's to read.
            Ask::Add { text } => match intake::parse(&text) {
                Ok(Source::Link(_)) => Command::Add(text),
                _ => return Err("Give a magnet link, an info-hash or the web address of a .torrent file."),
            },
            Ask::Pause { keys } => Command::Pause(checked(keys)?),
            Ask::Resume { keys } => Command::Resume(checked(keys)?),
            Ask::Remove { keys, delete_files } => Command::Remove {
                keys: checked(keys)?,
                delete_files,
            },
            Ask::Limits { down_kib, up_kib } => Command::SetLimits { down_kib, up_kib },
            Ask::Files { key, files } => {
                if files.len() > MAX_FILES {
                    return Err("Too many files at once.");
                }
                if files.is_empty() {
                    return Err("Choose at least one file.");
                }
                Command::SetFiles { key, files }
            }
            Ask::Category { keys, category } => {
                if category.as_ref().is_some_and(|c| c.chars().count() > MAX_CATEGORY) {
                    return Err("That category name is too long.");
                }
                Command::SetCategory {
                    keys: checked(keys)?,
                    category,
                }
            }
            Ask::SlowMode { on } => Command::SetSlowMode(on),
            Ask::Rules(r) => {
                let all = [
                    r.slow_down_kib,
                    r.slow_up_kib,
                    r.ratio_limit_pct,
                    r.seed_time_min,
                    r.seed_idle_min,
                    r.max_downloads,
                    r.max_seeding,
                    r.max_active,
                ];
                if all.iter().flatten().any(|&n| n > MAX_RULE) {
                    return Err("One of those numbers is too large.");
                }
                let mut limits = settings.limits;
                let keep = |new: Option<u32>, old: &mut u32| {
                    if let Some(n) = new {
                        *old = n;
                    }
                };
                keep(r.slow_down_kib, &mut limits.slow.down_kib);
                keep(r.slow_up_kib, &mut limits.slow.up_kib);
                keep(r.seed_time_min, &mut limits.seed_time_min);
                keep(r.seed_idle_min, &mut limits.seed_idle_min);
                keep(r.max_seeding, &mut limits.max_seeding);
                keep(r.max_active, &mut limits.max_active);
                return Ok(vec![
                    Command::SetRules {
                        max_downloads: r.max_downloads.unwrap_or(settings.max_downloads),
                        ratio_limit_pct: r.ratio_limit_pct.unwrap_or(settings.ratio_limit_pct),
                        schedule: settings.schedule,
                    },
                    Command::SetLimitSettings(limits),
                ]);
            }
        }])
    }
}

/// Every category name in use or kept, sorted, once each.
fn category_names(snap: &Snapshot) -> Vec<&String> {
    let mut categories: Vec<&String> = snap
        .torrents
        .iter()
        .filter_map(|t| t.category.as_ref())
        .chain(snap.feeds.iter().filter_map(|f| f.feed.category.as_ref()))
        .chain(snap.settings.categories.iter())
        .chain(snap.settings.category_dirs.keys())
        .collect();
    categories.sort();
    categories.dedup();
    categories
}

fn state_json(snap: &Snapshot) -> serde_json::Value {
    let torrents: Vec<serde_json::Value> = snap
        .torrents
        .iter()
        .map(|t| {
            serde_json::json!({
                "key": t.key,
                "name": t.name,
                "state": t.state.label(),
                "error": t.error,
                "total": t.total,
                "done": t.done,
                "uploaded": t.uploaded,
                "down_bps": t.down_bps,
                "up_bps": t.up_bps,
                "peers": t.peers_live,
                "category": t.category,
                "queue_pos": t.queue_pos,
                "ratio": crate::format::ratio(t.uploaded, t.done),
                "eta": crate::format::eta(t.total.saturating_sub(t.done), t.down_bps),
                "peers_seen": t.peers_seen,
                "files": t.files.len(),
            })
        })
        .collect();
    let categories = category_names(snap);
    let notices: Vec<serde_json::Value> = snap
        .notices
        .iter()
        .rev()
        .take(5)
        .map(|n| serde_json::json!({ "at": n.at, "text": n.text, "error": n.error }))
        .collect();
    serde_json::json!({
        "version": crate::VERSION,
        "down_bps": snap.down_bps,
        "up_bps": snap.up_bps,
        "down_limit_kib": snap.settings.down_limit_kib,
        "up_limit_kib": snap.settings.up_limit_kib,
        "blocked": snap.blocked,
        "schedule_off": snap.schedule_off,
        "torrents": torrents,
        "notices": notices,
        "categories": categories,
        "slow": {
            "on": snap.slow.is_some(),
            "by_schedule": snap.slow == Some(SlowReason::Schedule),
        },
        "rules": {
            "slow_down_kib": snap.settings.limits.slow.down_kib,
            "slow_up_kib": snap.settings.limits.slow.up_kib,
            "ratio_limit_pct": snap.settings.ratio_limit_pct,
            "seed_time_min": snap.settings.limits.seed_time_min,
            "seed_idle_min": snap.settings.limits.seed_idle_min,
            "max_downloads": snap.settings.max_downloads,
            "max_seeding": snap.settings.limits.max_seeding,
            "max_active": snap.settings.limits.max_active,
        },
    })
}

/// The files of one torrent, for choosing which to download.
fn files_json(t: &crate::model::TorrentView) -> serde_json::Value {
    let files: Vec<serde_json::Value> = t
        .files
        .iter()
        .enumerate()
        .map(|(i, f)| {
            serde_json::json!({
                "name": f.name,
                "len": f.len,
                "done": t.file_done.get(i).copied().unwrap_or(0),
                "chosen": t.selected.as_ref().is_none_or(|s| s.contains(&i)),
            })
        })
        .collect();
    serde_json::json!({ "key": t.key, "name": t.name, "files": files })
}

/// The info-hash a torrent is known by: 40 hex digits.
fn is_key(text: &str) -> bool {
    text.len() == 40 && text.bytes().all(|b| b.is_ascii_hexdigit())
}

impl Server {
    fn signed_in(&self, request: &Request) -> bool {
        let Some(token) = request.cookie() else {
            return false;
        };
        let mut sessions = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        sessions.retain(|_, until| *until > now);
        sessions.contains_key(token)
    }

    /// Checks a password, at most once a second, with a 30-second lock after every fifth
    /// wrong one.
    async fn check_password(&self, password: String) -> Check {
        {
            // Counted before the password is looked at so that tries sent at the same
            // moment cannot get past it.
            let mut guard = self.guard.lock().unwrap_or_else(|e| e.into_inner());
            let now = Instant::now();
            if guard.locked_until.is_some_and(|until| until > now) {
                return Check::Wait;
            }
            guard.locked_until = Some(now + Duration::from_secs(1));
        }
        let (salt, hash) = (self.salt.clone(), self.hash.clone());
        let right = tokio::task::spawn_blocking(move || password_matches(&salt, &hash, &password))
            .await
            .unwrap_or(false);
        let mut guard = self.guard.lock().unwrap_or_else(|e| e.into_inner());
        if !right {
            guard.failures += 1;
            if guard.failures.is_multiple_of(5) {
                guard.locked_until = Some(Instant::now() + Duration::from_secs(30));
            }
            return Check::Wrong;
        }
        guard.failures = 0;
        guard.locked_until = None;
        Check::Right
    }

    /// Starts a session and returns its token. The oldest one goes when there are too many.
    fn new_session(&self) -> String {
        let token = hex(&random(32));
        let mut sessions = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
        if sessions.len() >= MAX_SESSIONS {
            if let Some(oldest) = sessions.iter().min_by_key(|(_, until)| **until).map(|(t, _)| t.clone()) {
                sessions.remove(&oldest);
            }
        }
        sessions.insert(token.clone(), Instant::now() + SESSION_LIFE);
        token
    }

    fn end_session(&self, request: &Request) {
        if let Some(token) = request.cookie() {
            self.sessions.lock().unwrap_or_else(|e| e.into_inner()).remove(token);
        }
    }

    async fn login(&self, request: &Request) -> Reply {
        #[derive(Deserialize)]
        struct Login {
            password: String,
        }
        let Ok(login) = serde_json::from_slice::<Login>(&request.body) else {
            return Reply::error(400, "The request could not be read.");
        };
        match self.check_password(login.password).await {
            Check::Wait => Reply::error(429, "Too many tries. Wait a little and try again."),
            Check::Wrong => Reply::error(401, "Wrong password."),
            Check::Right => Reply {
                cookie: Some(self.new_session()),
                ..Reply::ok()
            },
        }
    }

    /// The host and origin checks every request has to pass, sessions aside.
    fn gate(&self, request: &Request) -> Result<(), Reply> {
        let Some(host) = request.header("host").filter(|h| host_allowed(h)) else {
            return Err(Reply::error(
                403,
                "Open this page by the computer's IP address, by localhost or by its .local name.",
            ));
        };
        if request.method == "POST" {
            let scheme = if self.https { "https" } else { "http" };
            let same_site = request
                .header("origin")
                .is_none_or(|origin| origin.eq_ignore_ascii_case(&format!("{scheme}://{host}")));
            // Programs that use the qBittorrent API cannot send the page's header. Their
            // session cookie is never sent along from another site, which keeps them safe.
            let ours = is_qbit(&request.path) || request.header("x-rusttorrent") == Some("1");
            if !ours || !same_site {
                return Err(Reply::error(
                    403,
                    "This request did not come from the RustTorrent page.",
                ));
            }
        }
        Ok(())
    }

    fn upload(&self, request: &Request) -> Reply {
        let name = match librqbit::torrent_from_bytes(&request.body) {
            Ok(t) => t
                .info
                .data
                .name
                .as_ref()
                .map(|n| String::from_utf8_lossy(n.as_ref()).into_owned())
                .unwrap_or_else(|| t.info_hash.as_string()),
            Err(_) => return Reply::error(400, "This file is not a torrent."),
        };
        let _ = self.tx.send(Command::AddTorrentBytes {
            bytes: request.body.clone(),
            category: None,
            paused: false,
        });
        Reply::json(200, serde_json::json!({ "ok": true, "name": name }))
    }

    async fn answer(&self, request: &Request) -> Reply {
        if let Err(reply) = self.gate(request) {
            return reply;
        }
        match (request.method.as_str(), request.path.as_str()) {
            ("GET", "/") => Reply::file("text/html; charset=utf-8", PAGE),
            ("GET", "/app.js") => Reply::file("text/javascript; charset=utf-8", SCRIPT),
            ("GET", "/app.css") => Reply::file("text/css; charset=utf-8", STYLE),
            ("POST", "/api/login") => self.login(request).await,
            (_, path) if is_qbit(path) => self.qbit(request).await,
            ("POST", "/api/logout") => {
                self.end_session(request);
                Reply {
                    cookie: Some(String::new()),
                    ..Reply::ok()
                }
            }
            (_, path) if path.starts_with("/api/") && !self.signed_in(request) => Reply::error(401, "Sign in first."),
            ("POST", "/api/upload") => self.upload(request),
            ("GET", path) if path.starts_with("/api/files/") => {
                let key = &path["/api/files/".len()..];
                if !is_key(key) {
                    return Reply::error(404, "There is nothing at this address.");
                }
                let snap = self.shared.lock().unwrap_or_else(|e| e.into_inner()).clone();
                match snap.torrents.iter().find(|t| t.key.eq_ignore_ascii_case(key)) {
                    Some(t) => Reply::json(200, files_json(t)),
                    None => Reply::error(404, "That torrent is not in the list."),
                }
            }
            ("GET", "/api/state") => {
                let snap = self.shared.lock().unwrap_or_else(|e| e.into_inner()).clone();
                Reply::json(200, state_json(&snap))
            }
            ("POST", "/api/command") => {
                let Ok(ask) = serde_json::from_slice::<Ask>(&request.body) else {
                    return Reply::error(400, "The request could not be read.");
                };
                let settings = self.shared.lock().unwrap_or_else(|e| e.into_inner()).settings.clone();
                match ask.into_commands(&settings) {
                    Ok(commands) => {
                        for command in commands {
                            let _ = self.tx.send(command);
                        }
                        Reply::ok()
                    }
                    Err(text) => Reply::error(400, text),
                }
            }
            _ => Reply::error(404, "There is nothing at this address."),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{State, TorrentView};

    #[test]
    fn hex_round_trip() {
        assert_eq!(unhex(&hex(&[0, 1, 0xab, 0xff])), Some(vec![0, 1, 0xab, 0xff]));
        assert_eq!(unhex("abc"), None);
        assert_eq!(unhex("zz"), None);
        assert_eq!(unhex("éé"), None);
    }

    #[test]
    fn hosts() {
        for good in [
            "localhost",
            "LOCALHOST:8787",
            "127.0.0.1:8787",
            "192.168.1.20",
            "[::1]:8787",
            "my-mac.local:8787",
        ] {
            assert!(host_allowed(good), "{good}");
        }
        for bad in [
            "example.org",
            "example.org:8787",
            "localhost.example.org",
            "127.0.0.1.example.org",
            "local",
            "",
        ] {
            assert!(!host_allowed(bad), "{bad}");
        }
    }

    #[test]
    fn heads() {
        let (r, len) = parse_head(
            "POST /api/command?x=1 HTTP/1.1\r\nHost: localhost:1\r\nContent-Length: 5\r\nCookie: a=b; rusttorrent=abc",
        )
        .unwrap();
        assert_eq!((r.method.as_str(), r.path.as_str(), len), ("POST", "/api/command", 5));
        assert_eq!(r.header("host"), Some("localhost:1"));
        assert_eq!(r.cookie(), Some("abc"));

        assert_eq!(parse_head("GET / HTTP/1.1\r\nHost: a\r\nHost: b"), Err(400));
        assert_eq!(
            parse_head("POST / HTTP/1.1\r\nContent-Length: 1\r\nContent-Length: 2"),
            Err(400)
        );
        assert_eq!(parse_head("POST / HTTP/1.1\r\nTransfer-Encoding: chunked"), Err(400));
        assert!(parse_head("POST / HTTP/1.1\r\nContent-Length: 999999").is_ok());
        assert_eq!(parse_head("POST / HTTP/1.1\r\nContent-Length: 99999999"), Err(413));
        assert_eq!(parse_head("GET http://x/ HTTP/1.1"), Err(400));
        assert_eq!(parse_head("nonsense"), Err(400));
    }

    #[test]
    fn asks() {
        let settings = Settings::default();
        let asks = |text: &str| {
            serde_json::from_str::<Ask>(text)
                .map_err(|_| "unreadable")
                .and_then(|a| a.into_commands(&settings))
        };
        let ask = |text: &str| asks(text).map(|mut c| c.remove(0));
        assert!(matches!(
            ask(r#"{"do":"add","text":"magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567"}"#),
            Ok(Command::Add(_))
        ));
        assert!(ask(r#"{"do":"add","text":"/etc/passwd"}"#).is_err());
        assert!(ask(r#"{"do":"add","text":"file:///tmp/a.torrent"}"#).is_err());
        assert!(matches!(
            ask(r#"{"do":"remove","keys":["a"]}"#),
            Ok(Command::Remove {
                delete_files: false,
                ..
            })
        ));
        assert!(matches!(
            ask(r#"{"do":"limits","down_kib":10,"up_kib":0}"#),
            Ok(Command::SetLimits {
                down_kib: 10,
                up_kib: 0
            })
        ));
        assert!(matches!(
            ask(r#"{"do":"files","key":"a","files":[0,2]}"#),
            Ok(Command::SetFiles { files, .. }) if files == [0, 2]
        ));
        assert!(ask(r#"{"do":"files","key":"a","files":[]}"#).is_err());
        assert!(matches!(
            ask(r#"{"do":"category","keys":["a"],"category":null}"#),
            Ok(Command::SetCategory { category: None, .. })
        ));
        let long = format!(r#"{{"do":"category","keys":["a"],"category":"{}"}}"#, "x".repeat(101));
        assert!(ask(&long).is_err());
        assert!(matches!(
            ask(r#"{"do":"slow_mode","on":true}"#),
            Ok(Command::SetSlowMode(true))
        ));
        // Rules change only what is given, and keep the schedule.
        let rules = asks(r#"{"do":"rules","max_downloads":3,"seed_time_min":60,"slow_up_kib":5}"#).unwrap();
        assert!(matches!(
            &rules[0],
            Command::SetRules { max_downloads: 3, ratio_limit_pct: 0, schedule } if *schedule == settings.schedule
        ));
        let Command::SetLimitSettings(limits) = &rules[1] else {
            panic!("{rules:?}")
        };
        assert_eq!((limits.seed_time_min, limits.slow.up_kib), (60, 5));
        assert_eq!(limits.slow.down_kib, settings.limits.slow.down_kib);
        assert_eq!(limits.max_active, settings.limits.max_active);
        assert!(asks(r#"{"do":"rules","max_active":99999999}"#).is_err());
        assert!(asks(r#"{"do":"rules","schedule":{}}"#).is_err());
        // Not one of the things the remote may do.
        assert!(ask(r#"{"do":"set_interface","name":"en0"}"#).is_err());
    }

    /// Sends one request to a running server and returns the status, head and body.
    async fn send(port: u16, text: String) -> (u16, String, String) {
        let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        stream.write_all(text.as_bytes()).await.unwrap();
        let mut out = String::new();
        stream.read_to_string(&mut out).await.unwrap();
        let (head, body) = out.split_once("\r\n\r\n").unwrap();
        let status = head.split(' ').nth(1).unwrap().parse().unwrap();
        (status, head.to_owned(), body.to_owned())
    }

    fn post(port: u16, path: &str, extra: &str, body: &str) -> String {
        format!(
            "POST {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n{extra}Content-Length: {}\r\n\r\n{body}",
            body.len()
        )
    }

    #[tokio::test]
    async fn a_whole_visit() {
        let (salt, hash) = hash_password("correct horse");
        let listener = listen(false, 0).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let shared: Shared = Arc::new(Mutex::new(Arc::new(Snapshot::default())));
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let server = tokio::spawn(serve(
            listener,
            unhex(&salt).unwrap(),
            unhex(&hash).unwrap(),
            shared,
            tx,
            None,
        ));
        let ours = "X-RustTorrent: 1\r\n";

        // The page needs no password; the list does.
        let (status, head, body) = send(port, format!("GET / HTTP/1.1\r\nHost: localhost:{port}\r\n\r\n")).await;
        assert_eq!(status, 200);
        assert!(head.contains("Content-Security-Policy") && body.contains("RustTorrent"));
        let state = format!("GET /api/state HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n");
        assert_eq!(send(port, format!("{state}\r\n")).await.0, 401);

        // A domain name pointed at this machine is turned away.
        let (status, ..) = send(port, "GET / HTTP/1.1\r\nHost: evil.example\r\n\r\n".into()).await;
        assert_eq!(status, 403);

        // Signing in needs the page's own header, the right origin and the right password.
        let good = r#"{"password":"correct horse"}"#;
        assert_eq!(send(port, post(port, "/api/login", "", good)).await.0, 403);
        let foreign = format!("{ours}Origin: http://evil.example\r\n");
        assert_eq!(send(port, post(port, "/api/login", &foreign, good)).await.0, 403);
        let (status, ..) = send(port, post(port, "/api/login", ours, r#"{"password":"wrong"}"#)).await;
        assert_eq!(status, 401);
        // A second try straight after a wrong one has to wait.
        assert_eq!(send(port, post(port, "/api/login", ours, good)).await.0, 429);
        tokio::time::sleep(Duration::from_millis(1100)).await;
        let own = format!("{ours}Origin: http://127.0.0.1:{port}\r\n");
        let (status, head, _) = send(port, post(port, "/api/login", &own, good)).await;
        assert_eq!(status, 200);
        let cookie = head
            .lines()
            .find_map(|l| l.strip_prefix("Set-Cookie: "))
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_owned();
        assert!(head.contains("HttpOnly; SameSite=Strict"));

        // With the session the list can be read and a command sent.
        let signed = format!("Cookie: {cookie}\r\n");
        let (status, _, body) = send(port, format!("{state}{signed}\r\n")).await;
        assert_eq!(status, 200);
        assert!(body.contains("\"torrents\":[]"));
        let pause = r#"{"do":"pause","keys":["abc"]}"#;
        assert_eq!(send(port, post(port, "/api/command", ours, pause)).await.0, 401);
        let both = format!("{ours}{signed}");
        assert_eq!(send(port, post(port, "/api/command", &both, pause)).await.0, 200);
        assert!(matches!(rx.try_recv(), Ok(Command::Pause(keys)) if keys == ["abc"]));
        // The cookie alone, as another site's form would send it, is not enough.
        assert_eq!(send(port, post(port, "/api/command", &signed, pause)).await.0, 403);
        assert!(rx.try_recv().is_err());

        // A torrent file can be sent; anything else is turned away.
        let torrent = "d4:infod6:lengthi1e4:name5:a.txt12:piece lengthi16384e6:pieces20:aaaaaaaaaaaaaaaaaaaaee";
        let (status, _, body) = send(port, post(port, "/api/upload", &both, torrent)).await;
        assert_eq!(status, 200, "{body}");
        assert!(body.contains("a.txt"));
        assert!(matches!(rx.try_recv(), Ok(Command::AddTorrentBytes { bytes, .. }) if bytes == torrent.as_bytes()));
        let (status, _, body) = send(port, post(port, "/api/upload", &both, "hello")).await;
        assert_eq!(status, 400);
        assert!(body.contains("not a torrent"));
        assert_eq!(send(port, post(port, "/api/upload", ours, torrent)).await.0, 401);
        assert!(rx.try_recv().is_err());
        // A large body is refused before it is read unless the request is signed in.
        let big =
            format!("POST /api/upload HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n{ours}Content-Length: 100000\r\n\r\n");
        assert_eq!(send(port, big).await.0, 413);
        let large =
            format!("POST /api/command HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n{both}Content-Length: 100000\r\n\r\n");
        assert_eq!(send(port, large).await.0, 413);
        // Files of a torrent not in the list, and an address that is not a key.
        let files = format!(
            "GET /api/files/{} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n{signed}\r\n",
            "a".repeat(40)
        );
        assert_eq!(send(port, files).await.0, 404);
        let files = format!("GET /api/files/../x HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n{signed}\r\n");
        assert_eq!(send(port, files).await.0, 404);

        // Signing out ends the session.
        assert_eq!(send(port, post(port, "/api/logout", &both, "")).await.0, 200);
        assert_eq!(send(port, format!("{state}{signed}\r\n")).await.0, 401);
        server.abort();
    }

    /// The qBittorrent API as Sonarr or a phone app would use it.
    #[tokio::test]
    async fn a_qbittorrent_visit() {
        let (salt, hash) = hash_password("correct horse");
        let listener = listen(false, 0).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let key = "ab".repeat(20);
        let mut seeding = TorrentView::sample(&key, State::Seeding, 10, 10);
        seeding.category = Some("tv".into());
        seeding.up_bps = 3;
        let other = TorrentView::sample(&"cd".repeat(20), State::Paused, 0, 10);
        let snap = Snapshot {
            torrents: vec![seeding, other],
            ..Snapshot::default()
        };
        let shared: Shared = Arc::new(Mutex::new(Arc::new(snap)));
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let server = tokio::spawn(serve(
            listener,
            unhex(&salt).unwrap(),
            unhex(&hash).unwrap(),
            shared,
            tx,
            None,
        ));
        let form = "Content-Type: application/x-www-form-urlencoded\r\n";
        let get = |path: &str, extra: &str| format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n{extra}\r\n");

        // Nothing without signing in; a wrong password is "Fails.", as qBittorrent says it.
        let (status, _, body) = send(port, get("/api/v2/app/version", "")).await;
        assert_eq!((status, body.as_str()), (403, "Forbidden"));
        let (status, _, body) = send(
            port,
            post(port, "/api/v2/auth/login", form, "username=admin&password=no"),
        )
        .await;
        assert_eq!((status, body.as_str()), (200, "Fails."));
        // A login from another site's page is refused before the password is looked at.
        let foreign = format!("{form}Origin: http://evil.example\r\n");
        let good = "username=admin&password=correct+horse";
        assert_eq!(
            send(port, post(port, "/api/v2/auth/login", &foreign, good)).await.0,
            403
        );
        tokio::time::sleep(Duration::from_millis(1100)).await;
        let (status, head, body) = send(port, post(port, "/api/v2/auth/login", form, good)).await;
        assert_eq!((status, body.as_str()), (200, "Ok."));
        let cookie = head
            .lines()
            .find_map(|l| l.strip_prefix("Set-Cookie: "))
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_owned();
        assert!(cookie.starts_with("SID="), "{cookie}");
        let signed = format!("Cookie: {cookie}\r\n");

        let (_, _, body) = send(port, get("/api/v2/app/webapiVersion", &signed)).await;
        assert_eq!(body, "2.9.3");
        let (status, _, body) = send(port, get("/api/v2/torrents/info?category=tv", &signed)).await;
        assert_eq!(status, 200);
        let list: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(list.as_array().unwrap().len(), 1);
        assert_eq!(list[0]["hash"], key.as_str());
        assert_eq!(list[0]["state"], "uploading");
        assert_eq!(list[0]["category"], "tv");
        let (_, _, body) = send(port, get("/api/v2/torrents/info?filter=paused", &signed)).await;
        assert!(body.contains("pausedDL") && !body.contains(&key), "{body}");
        let (_, _, body) = send(port, get("/api/v2/sync/maindata?rid=4", &signed)).await;
        let main: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(main["rid"], 5);
        assert_eq!(main["torrents"].as_object().unwrap().len(), 2);
        assert!(main["categories"]["tv"].is_object());

        // Changing something needs a POST.
        let pause = format!("hashes={key}%7C{}", "0".repeat(40));
        let (status, ..) = send(port, get(&format!("/api/v2/torrents/pause?{pause}"), &signed)).await;
        assert_eq!(status, 405);
        let both = format!("{form}{signed}");
        assert_eq!(
            send(port, post(port, "/api/v2/torrents/pause", &both, &pause)).await.0,
            200
        );
        assert!(matches!(rx.try_recv(), Ok(Command::Pause(keys)) if keys == [key.clone()]));
        // From another site, even with the cookie, it is refused.
        let foreign = format!("{both}Origin: http://evil.example\r\n");
        assert_eq!(
            send(port, post(port, "/api/v2/torrents/pause", &foreign, &pause))
                .await
                .0,
            403
        );
        assert!(rx.try_recv().is_err());

        let limits = format!("hashes={key}&ratioLimit=2&seedingTimeLimit=-1&inactiveSeedingTimeLimit=-2");
        assert_eq!(
            send(port, post(port, "/api/v2/torrents/setShareLimits", &both, &limits))
                .await
                .0,
            200
        );
        assert!(matches!(
            rx.try_recv(),
            Ok(Command::SetRatioLimit { limit: Some(200), .. })
        ));
        assert!(matches!(
            rx.try_recv(),
            Ok(Command::SetSeedingLimits {
                time_limit_min: Some(0),
                idle_limit_min: None,
                ..
            })
        ));

        // Adding: a link with a category, then a torrent file as Sonarr sends it.
        let magnet = format!("magnet:?xt=urn:btih:{}", "ef".repeat(20));
        let add = format!(
            "urls={}&category=tv&paused=true",
            magnet.replace(':', "%3A").replace('?', "%3F")
        );
        let (status, _, body) = send(port, post(port, "/api/v2/torrents/add", &both, &add)).await;
        assert_eq!((status, body.as_str()), (200, "Ok."));
        assert!(matches!(rx.try_recv(),
            Ok(Command::AddWith(o)) if o.text == magnet && o.category.as_deref() == Some("tv") && o.paused));
        let torrent = "d4:infod6:lengthi1e4:name5:a.txt12:piece lengthi16384e6:pieces20:aaaaaaaaaaaaaaaaaaaaee";
        let body = format!(
            "--b0und\r\nContent-Disposition: form-data; name=\"torrents\"; filename=\"a.torrent\"\r\n\
             Content-Type: application/x-bittorrent\r\n\r\n{torrent}\r\n--b0und--\r\n"
        );
        let multipart = format!("Content-Type: multipart/form-data; boundary=b0und\r\n{signed}");
        let (status, _, reply) = send(port, post(port, "/api/v2/torrents/add", &multipart, &body)).await;
        assert_eq!((status, reply.as_str()), (200, "Ok."));
        assert!(matches!(rx.try_recv(), Ok(Command::AddTorrentBytes { bytes, .. }) if bytes == torrent.as_bytes()));
        // A path on this computer is not something the API may add.
        let (status, ..) = send(port, post(port, "/api/v2/torrents/add", &both, "urls=%2Fetc%2Fpasswd")).await;
        assert_eq!(status, 415);
        assert!(rx.try_recv().is_err());
        assert_eq!(
            send(port, post(port, "/api/v2/torrents/setLocation", &both, &pause))
                .await
                .0,
            403
        );
        assert_eq!(send(port, get("/api/v2/no/such", &signed)).await.0, 404);

        // Signing out ends the session.
        assert_eq!(send(port, post(port, "/api/v2/auth/logout", &both, "")).await.0, 200);
        assert_eq!(send(port, get("/api/v2/app/version", &signed)).await.0, 403);
        server.abort();
    }
}
