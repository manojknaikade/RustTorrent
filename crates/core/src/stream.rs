//! Plays a file while it downloads. A small web server on this computer alone hands each
//! file to a player such as VLC, which asks for parts of it with `Range` requests. The
//! engine fetches the parts a player asks for before the others.
//!
//! Each link carries a long random token, so another program or a web page cannot guess
//! one. The server listens on 127.0.0.1 only and starts the first time a file is played.

use std::{
    collections::VecDeque,
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
    time::Duration,
};

use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncSeek, AsyncSeekExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::Semaphore,
    task::JoinHandle,
};

/// Each stream holds one of the engine's eight slots for disk work, so a few at most.
const MAX_STREAMS: usize = 3;
/// Links kept. The oldest goes first.
const MAX_LINKS: usize = 8;
const MAX_HEADER: usize = 8 * 1024;
const READ_TIMEOUT: Duration = Duration::from_secs(10);
/// How long a request waits for a free stream before it gives up.
const WAIT_FOR_SLOT: Duration = Duration::from_secs(15);

pub(crate) trait Reader: AsyncRead + AsyncSeek + Unpin + Send {}
impl<T: AsyncRead + AsyncSeek + Unpin + Send> Reader for T {}

type Opened = anyhow::Result<(Box<dyn Reader>, u64)>;
/// Opens the file from its start and says how long it is.
pub(crate) type Open = Arc<dyn Fn() -> Pin<Box<dyn Future<Output = Opened> + Send>> + Send + Sync>;

/// A file that can be played, for the window to open or copy.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct StreamLink {
    pub key: String,
    pub file: usize,
    pub url: String,
}

struct Link {
    key: String,
    file: usize,
    token: String,
    name: String,
    open: Open,
}

type Links = Arc<Mutex<VecDeque<Link>>>;

pub(crate) struct Streams {
    port: u16,
    links: Links,
    task: JoinHandle<()>,
}

impl Drop for Streams {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Streams {
    pub(crate) async fn start() -> std::io::Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
        let port = listener.local_addr()?.port();
        let links: Links = Arc::default();
        let slots = Arc::new(Semaphore::new(MAX_STREAMS));
        let shared = links.clone();
        let task = tokio::spawn(async move {
            loop {
                let Ok((socket, _)) = listener.accept().await else {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    continue;
                };
                let (links, slots) = (shared.clone(), slots.clone());
                tokio::spawn(async move {
                    let _ = serve(socket, &links, &slots).await;
                });
            }
        });
        Ok(Self { port, links, task })
    }

    /// Adds a link for this file, or keeps the one it has, and returns its address.
    pub(crate) fn add(&self, key: &str, file: usize, name: &str, open: Open) -> String {
        let mut links = self.links.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(i) = links.iter().position(|l| l.key == key && l.file == file) {
            let mut link = links.remove(i).expect("the link just found");
            link.open = open;
            links.push_back(link);
        } else {
            if links.len() >= MAX_LINKS {
                links.pop_front();
            }
            links.push_back(Link {
                key: key.to_owned(),
                file,
                token: crate::remote::new_token(),
                name: name.to_owned(),
                open,
            });
        }
        self.url(links.back().expect("the link just added"))
    }

    /// Drops the links of a torrent that was removed, so it is not kept in memory.
    pub(crate) fn forget(&self, key: &str) {
        (self.links.lock().unwrap_or_else(|e| e.into_inner())).retain(|l| l.key != key);
    }

    pub(crate) fn views(&self) -> Vec<StreamLink> {
        let links = self.links.lock().unwrap_or_else(|e| e.into_inner());
        links
            .iter()
            .map(|l| StreamLink {
                key: l.key.clone(),
                file: l.file,
                url: self.url(l),
            })
            .collect()
    }

    fn url(&self, link: &Link) -> String {
        // The last part is only there so players show the file's name.
        let base = link.name.rsplit('/').next().unwrap_or(&link.name);
        format!("http://127.0.0.1:{}/{}/{}", self.port, link.token, encode(base))
    }
}

fn encode(text: &str) -> String {
    let mut out = String::new();
    for b in text.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// The type a player is told, from the file's extension.
fn content_type(name: &str) -> &'static str {
    let ext = name
        .rsplit_once('.')
        .map(|(_, e)| e.to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "mp4" | "m4v" => "video/mp4",
        "mkv" => "video/x-matroska",
        "webm" => "video/webm",
        "avi" => "video/x-msvideo",
        "mov" => "video/quicktime",
        "ts" => "video/mp2t",
        "mp3" => "audio/mpeg",
        "m4a" | "aac" => "audio/mp4",
        "flac" => "audio/flac",
        "ogg" | "oga" => "audio/ogg",
        "opus" => "audio/opus",
        "wav" => "audio/wav",
        "srt" => "application/x-subrip",
        "vtt" => "text/vtt",
        _ => "application/octet-stream",
    }
}

/// The bytes asked for, first and last included. `None` when the range cannot be met;
/// `Some(None)` when the whole file is wanted.
fn range(header: Option<&str>, len: u64) -> Option<Option<(u64, u64)>> {
    let Some(header) = header else { return Some(None) };
    let Some(spec) = header.trim().strip_prefix("bytes=") else {
        return Some(None);
    };
    if spec.contains(',') {
        // Several ranges at once are rare; the whole file answers them too.
        return Some(None);
    }
    let (first, last) = spec.split_once('-')?;
    let (first, last) = (first.trim(), last.trim());
    let pair = if first.is_empty() {
        let tail: u64 = last.parse().ok()?;
        if tail == 0 || len == 0 {
            return None;
        }
        (len.saturating_sub(tail), len - 1)
    } else {
        let first: u64 = first.parse().ok()?;
        let last = if last.is_empty() { u64::MAX } else { last.parse().ok()? };
        if first >= len || last < first {
            return None;
        }
        (first, last.min(len - 1))
    };
    Some(Some(pair))
}

struct Asked {
    head: bool,
    token: String,
    range: Option<String>,
}

async fn read_head(socket: &mut TcpStream) -> Result<Asked, u16> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 1024];
    let end = loop {
        if let Some(at) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break at;
        }
        if buf.len() > MAX_HEADER {
            return Err(431);
        }
        let n = match tokio::time::timeout(READ_TIMEOUT, socket.read(&mut chunk)).await {
            Ok(Ok(n)) if n > 0 => n,
            _ => return Err(0),
        };
        buf.extend_from_slice(&chunk[..n]);
    };
    let text = std::str::from_utf8(&buf[..end]).map_err(|_| 400u16)?;
    let mut lines = text.split("\r\n");
    let first = lines.next().unwrap_or_default();
    let mut parts = first.split(' ');
    let (method, path) = (parts.next().unwrap_or_default(), parts.next().unwrap_or_default());
    let head = match method {
        "GET" => false,
        "HEAD" => true,
        _ => return Err(405),
    };
    let token = path
        .trim_start_matches('/')
        .split(['/', '?'])
        .next()
        .unwrap_or_default();
    let range = lines.find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.trim()
            .eq_ignore_ascii_case("range")
            .then(|| value.trim().to_owned())
    });
    Ok(Asked {
        head,
        token: token.to_owned(),
        range,
    })
}

async fn reply(socket: &mut TcpStream, status: u16, extra: &str) -> std::io::Result<()> {
    let reason = match status {
        400 => "Bad Request",
        404 => "Not Found",
        405 => "Method Not Allowed",
        416 => "Range Not Satisfiable",
        431 => "Request Header Fields Too Large",
        503 => "Service Unavailable",
        _ => "Internal Server Error",
    };
    let text = format!("HTTP/1.1 {status} {reason}\r\nContent-Length: 0\r\nConnection: close\r\n{extra}\r\n");
    socket.write_all(text.as_bytes()).await
}

async fn serve(mut socket: TcpStream, links: &Links, slots: &Semaphore) -> std::io::Result<()> {
    let asked = match read_head(&mut socket).await {
        Ok(asked) => asked,
        Err(0) => return Ok(()),
        Err(status) => return reply(&mut socket, status, "").await,
    };
    let found = {
        let links = links.lock().unwrap_or_else(|e| e.into_inner());
        links
            .iter()
            .find(|l| !asked.token.is_empty() && l.token == asked.token)
            .map(|l| (l.name.clone(), l.open.clone()))
    };
    let Some((name, open)) = found else {
        return reply(&mut socket, 404, "").await;
    };
    let Ok(Ok(_slot)) = tokio::time::timeout(WAIT_FOR_SLOT, slots.acquire()).await else {
        return reply(&mut socket, 503, "Retry-After: 2\r\n").await;
    };
    let (mut file, len) = match open().await {
        Ok(opened) => opened,
        Err(_) => return reply(&mut socket, 500, "").await,
    };
    let Some(wanted) = range(asked.range.as_deref(), len) else {
        return reply(&mut socket, 416, &format!("Content-Range: bytes */{len}\r\n")).await;
    };
    let (status, first, count, extra) = match wanted {
        Some((first, last)) => (
            "206 Partial Content",
            first,
            last - first + 1,
            format!("Content-Range: bytes {first}-{last}/{len}\r\n"),
        ),
        None => ("200 OK", 0, len, String::new()),
    };
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {}\r\nContent-Length: {count}\r\nAccept-Ranges: bytes\r\n\
         Connection: close\r\n{extra}\r\n",
        content_type(&name)
    );
    socket.write_all(head.as_bytes()).await?;
    if asked.head || count == 0 {
        return Ok(());
    }
    file.seek(std::io::SeekFrom::Start(first)).await?;
    tokio::io::copy(&mut file.take(count), &mut socket).await?;
    socket.flush().await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranges() {
        assert_eq!(range(None, 100), Some(None));
        assert_eq!(range(Some("bytes=0-"), 100), Some(Some((0, 99))));
        assert_eq!(range(Some("bytes=10-19"), 100), Some(Some((10, 19))));
        assert_eq!(range(Some("bytes=90-500"), 100), Some(Some((90, 99))));
        assert_eq!(range(Some("bytes=-10"), 100), Some(Some((90, 99))));
        assert_eq!(range(Some("bytes=-500"), 100), Some(Some((0, 99))));
        assert_eq!(range(Some("bytes=100-"), 100), None);
        assert_eq!(range(Some("bytes=20-10"), 100), None);
        assert_eq!(range(Some("bytes=0-1,5-6"), 100), Some(None));
        assert_eq!(range(Some("items=0-1"), 100), Some(None));
        assert_eq!(content_type("a/Show.S01E01.MKV"), "video/x-matroska");
        assert_eq!(encode("a b+é.mkv"), "a%20b%2B%C3%A9.mkv");
    }

    async fn ask(port: u16, request: &str) -> Vec<u8> {
        let mut socket = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        socket.write_all(request.as_bytes()).await.unwrap();
        let mut out = Vec::new();
        socket.read_to_end(&mut out).await.unwrap();
        out
    }

    #[tokio::test]
    async fn a_player_visit() {
        let data: Arc<Vec<u8>> = Arc::new((0..=255u8).cycle().take(1000).collect());
        let source = data.clone();
        let open: Open = Arc::new(move || {
            let bytes = source.clone();
            Box::pin(async move {
                let len = bytes.len() as u64;
                let reader: Box<dyn Reader> = Box::new(std::io::Cursor::new(bytes.to_vec()));
                Ok((reader, len))
            })
        });
        let streams = Streams::start().await.unwrap();
        let url = streams.add("k", 2, "Show/Episode 1.mkv", open.clone());
        assert!(url.ends_with("/Episode%201.mkv"), "{url}");
        // The same file keeps its link.
        assert_eq!(streams.add("k", 2, "Show/Episode 1.mkv", open), url);
        assert_eq!(streams.views().len(), 1);
        let path = url.split_once(&format!("127.0.0.1:{}", streams.port)).unwrap().1;
        let port = streams.port;

        let whole = ask(port, &format!("GET {path} HTTP/1.1\r\nHost: x\r\n\r\n")).await;
        let text = String::from_utf8_lossy(&whole);
        assert!(text.starts_with("HTTP/1.1 200 OK"), "{text}");
        assert!(text.contains("Content-Length: 1000\r\n"));
        assert!(text.contains("Content-Type: video/x-matroska\r\n"));
        assert!(whole.ends_with(&data[..]));

        let part = ask(port, &format!("GET {path} HTTP/1.1\r\nrange: bytes=500-509\r\n\r\n")).await;
        let text = String::from_utf8_lossy(&part);
        assert!(text.starts_with("HTTP/1.1 206 Partial Content"), "{text}");
        assert!(text.contains("Content-Range: bytes 500-509/1000\r\n"));
        assert!(text.contains("Content-Length: 10\r\n"));
        assert!(part.ends_with(&data[500..510]));

        let head = ask(port, &format!("HEAD {path} HTTP/1.1\r\nRange: bytes=-4\r\n\r\n")).await;
        let text = String::from_utf8_lossy(&head);
        assert!(text.contains("Content-Range: bytes 996-999/1000\r\n"), "{text}");
        assert!(text.ends_with("\r\n\r\n"));

        let past = ask(port, &format!("GET {path} HTTP/1.1\r\nRange: bytes=1000-\r\n\r\n")).await;
        assert!(String::from_utf8_lossy(&past).starts_with("HTTP/1.1 416"));
        let wrong = ask(port, "GET /nottoken/x HTTP/1.1\r\n\r\n").await;
        assert!(String::from_utf8_lossy(&wrong).starts_with("HTTP/1.1 404"));
        let post = ask(port, &format!("POST {path} HTTP/1.1\r\n\r\n")).await;
        assert!(String::from_utf8_lossy(&post).starts_with("HTTP/1.1 405"));

        streams.forget("k");
        assert!(streams.views().is_empty());
        let gone = ask(port, &format!("GET {path} HTTP/1.1\r\n\r\n")).await;
        assert!(String::from_utf8_lossy(&gone).starts_with("HTTP/1.1 404"));
    }
}
