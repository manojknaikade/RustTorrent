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
//! The server speaks plain HTTP, one request per connection. There is no encryption.
use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::num::NonZeroU32;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use aws_lc_rs::pbkdf2;
use aws_lc_rs::rand::{SecureRandom, SystemRandom};
use serde::Deserialize;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc::UnboundedSender;

use crate::intake::{self, Source};
use crate::model::Snapshot;
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

struct Server {
    salt: Vec<u8>,
    hash: Vec<u8>,
    shared: Shared,
    tx: UnboundedSender<Command>,
    /// Session tokens and when each stops being valid.
    sessions: Mutex<HashMap<String, Instant>>,
    guard: Mutex<Guard>,
}

/// Answers requests until the task is dropped. Sessions live only as long as it does, so
/// changing the password signs everybody out.
pub(crate) async fn serve(
    listener: TcpListener,
    salt: Vec<u8>,
    hash: Vec<u8>,
    shared: Shared,
    tx: UnboundedSender<Command>,
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
        connections.spawn(async move {
            let _ = tokio::time::timeout(TIME_LIMIT, connection(&server, stream)).await;
        });
    }
}

async fn connection(server: &Server, mut stream: TcpStream) -> std::io::Result<()> {
    let reply = match read_request(&mut stream).await {
        Ok(request) => server.answer(&request).await,
        Err(status) => Reply::error(status, "The request could not be read."),
    };
    stream.write_all(&reply.into_bytes()).await?;
    stream.shutdown().await
}

#[derive(Debug, PartialEq, Eq)]
struct Request {
    method: String,
    /// Without the part after `?`.
    path: String,
    /// Names in lower case.
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Request {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(n, _)| n == name).map(|(_, v)| v.as_str())
    }

    fn cookie(&self) -> Option<&str> {
        self.header("cookie")?
            .split(';')
            .filter_map(|part| part.trim().split_once('='))
            .find(|(name, _)| *name == COOKIE)
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
    if length > MAX_BODY {
        return Err(413);
    }
    let request = Request {
        method: method.to_owned(),
        path: target.split('?').next().unwrap_or("/").to_owned(),
        headers,
        body: Vec::new(),
    };
    Ok((request, length))
}

async fn read_request(stream: &mut TcpStream) -> Result<Request, u16> {
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
}

impl Reply {
    fn file(kind: &'static str, body: &str) -> Self {
        Self {
            status: 200,
            kind,
            body: body.as_bytes().to_vec(),
            cookie: None,
        }
    }

    fn json(status: u16, value: serde_json::Value) -> Self {
        Self {
            status,
            kind: "application/json",
            body: value.to_string().into_bytes(),
            cookie: None,
        }
    }

    fn ok() -> Self {
        Self::json(200, serde_json::json!({ "ok": true }))
    }

    fn error(status: u16, text: &str) -> Self {
        Self::json(status, serde_json::json!({ "error": text }))
    }

    fn into_bytes(self) -> Vec<u8> {
        let reason = match self.status {
            200 => "OK",
            400 => "Bad Request",
            401 => "Unauthorized",
            403 => "Forbidden",
            404 => "Not Found",
            413 => "Content Too Large",
            429 => "Too Many Requests",
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
            head.push_str(&format!(
                "Set-Cookie: {COOKIE}={token}; Path=/; Max-Age={age}; HttpOnly; SameSite=Strict\r\n"
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
}

impl Ask {
    fn into_command(self) -> Result<Command, &'static str> {
        let checked = |keys: Vec<String>| {
            if keys.len() > MAX_KEYS {
                Err("Too many torrents at once.")
            } else {
                Ok(keys)
            }
        };
        Ok(match self {
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
        })
    }
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
            })
        })
        .collect();
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
    })
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

    async fn login(&self, request: &Request) -> Reply {
        #[derive(Deserialize)]
        struct Login {
            password: String,
        }
        let Ok(login) = serde_json::from_slice::<Login>(&request.body) else {
            return Reply::error(400, "The request could not be read.");
        };
        {
            // One try a second at most, counted before the password is looked at so that
            // tries sent at the same moment cannot get past it.
            let mut guard = self.guard.lock().unwrap_or_else(|e| e.into_inner());
            let now = Instant::now();
            if guard.locked_until.is_some_and(|until| until > now) {
                return Reply::error(429, "Too many tries. Wait a little and try again.");
            }
            guard.locked_until = Some(now + Duration::from_secs(1));
        }
        let (salt, hash) = (self.salt.clone(), self.hash.clone());
        let right = tokio::task::spawn_blocking(move || password_matches(&salt, &hash, &login.password))
            .await
            .unwrap_or(false);
        let mut guard = self.guard.lock().unwrap_or_else(|e| e.into_inner());
        if !right {
            guard.failures += 1;
            if guard.failures.is_multiple_of(5) {
                guard.locked_until = Some(Instant::now() + Duration::from_secs(30));
            }
            return Reply::error(401, "Wrong password.");
        }
        guard.failures = 0;
        guard.locked_until = None;
        drop(guard);

        let token = hex(&random(32));
        let mut sessions = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
        if sessions.len() >= MAX_SESSIONS {
            if let Some(oldest) = sessions.iter().min_by_key(|(_, until)| **until).map(|(t, _)| t.clone()) {
                sessions.remove(&oldest);
            }
        }
        sessions.insert(token.clone(), Instant::now() + SESSION_LIFE);
        Reply {
            cookie: Some(token),
            ..Reply::ok()
        }
    }

    async fn answer(&self, request: &Request) -> Reply {
        let Some(host) = request.header("host").filter(|h| host_allowed(h)) else {
            return Reply::error(
                403,
                "Open this page by the computer's IP address, by localhost or by its .local name.",
            );
        };
        if request.method == "POST" {
            let same_site = request
                .header("origin")
                .is_none_or(|origin| origin.eq_ignore_ascii_case(&format!("http://{host}")));
            if request.header("x-rusttorrent") != Some("1") || !same_site {
                return Reply::error(403, "This request did not come from the RustTorrent page.");
            }
        }
        match (request.method.as_str(), request.path.as_str()) {
            ("GET", "/") => Reply::file("text/html; charset=utf-8", PAGE),
            ("GET", "/app.js") => Reply::file("text/javascript; charset=utf-8", SCRIPT),
            ("GET", "/app.css") => Reply::file("text/css; charset=utf-8", STYLE),
            ("POST", "/api/login") => self.login(request).await,
            ("POST", "/api/logout") => {
                if let Some(token) = request.cookie() {
                    self.sessions.lock().unwrap_or_else(|e| e.into_inner()).remove(token);
                }
                Reply {
                    cookie: Some(String::new()),
                    ..Reply::ok()
                }
            }
            ("GET", "/api/state") | ("POST", "/api/command") if !self.signed_in(request) => {
                Reply::error(401, "Sign in first.")
            }
            ("GET", "/api/state") => {
                let snap = self.shared.lock().unwrap_or_else(|e| e.into_inner()).clone();
                Reply::json(200, state_json(&snap))
            }
            ("POST", "/api/command") => {
                let Ok(ask) = serde_json::from_slice::<Ask>(&request.body) else {
                    return Reply::error(400, "The request could not be read.");
                };
                match ask.into_command() {
                    Ok(command) => {
                        let _ = self.tx.send(command);
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
        assert_eq!(parse_head("POST / HTTP/1.1\r\nContent-Length: 999999"), Err(413));
        assert_eq!(parse_head("GET http://x/ HTTP/1.1"), Err(400));
        assert_eq!(parse_head("nonsense"), Err(400));
    }

    #[test]
    fn asks() {
        let ask = |text: &str| {
            serde_json::from_str::<Ask>(text)
                .map_err(|_| "unreadable")
                .and_then(Ask::into_command)
        };
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
        // Not one of the things the remote may do.
        assert!(ask(r#"{"do":"set_interface","name":"en0"}"#).is_err());
    }

    /// Sends one request to a running server and returns the status, head and body.
    async fn send(port: u16, text: String) -> (u16, String, String) {
        let mut stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
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

        // Signing out ends the session.
        assert_eq!(send(port, post(port, "/api/logout", &both, "")).await.0, 200);
        assert_eq!(send(port, format!("{state}{signed}\r\n")).await.0, 401);
        server.abort();
    }
}
