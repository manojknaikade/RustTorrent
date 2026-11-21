//! Takes over the torrents of Transmission or qBittorrent: their `.torrent` files, the
//! folder each one saves to, its category and whether it was paused. The data is not
//! moved; the engine checks it where it is and goes on from there.
//!
//! The keys read from the other apps' resume files are written from knowledge of their
//! formats, and checked here against hand-made files only, not against files written by
//! the apps themselves.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The app to take torrents from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Client {
    Transmission,
    QBittorrent,
}

impl Client {
    pub fn name(self) -> &'static str {
        match self {
            Client::Transmission => "Transmission",
            Client::QBittorrent => "qBittorrent",
        }
    }

    /// Where the app keeps its torrents, on this system.
    pub fn default_dir(self) -> Option<PathBuf> {
        let home = dirs::home_dir()?;
        let dir = match (self, cfg!(target_os = "macos")) {
            (Client::Transmission, true) => home.join("Library/Application Support/Transmission"),
            (Client::QBittorrent, true) => home.join("Library/Application Support/qBittorrent/BT_backup"),
            (Client::Transmission, false) => dirs::config_dir()?.join("transmission"),
            (Client::QBittorrent, false) => dirs::data_local_dir()?.join("qBittorrent/BT_backup"),
        };
        Some(dir)
    }
}

/// One torrent found in the other app.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    /// Its `.torrent` file.
    pub torrent: PathBuf,
    /// The folder the other app saves it in. A torrent with several files has its own
    /// folder inside this one; see [`crate::manage::data_folder`].
    pub save_path: Option<PathBuf>,
    pub category: Option<String>,
    pub paused: bool,
}

/// What a look through the other app's folder found.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Scan {
    pub found: Vec<Found>,
    /// Torrents known only by a magnet link, with no `.torrent` file to add.
    pub skipped: usize,
}

/// Looks through `dir`, the folder the other app keeps its torrents in.
pub fn scan(client: Client, dir: &Path) -> Result<Scan, String> {
    match client {
        Client::Transmission => scan_transmission(dir),
        Client::QBittorrent => scan_qbittorrent(dir),
    }
}

/// Transmission keeps `torrents/<stem>.torrent` and `resume/<stem>.resume` (with capital
/// letters before version 4). The resume file holds `destination` and `paused`.
fn scan_transmission(dir: &Path) -> Result<Scan, String> {
    let torrents = child(dir, &["torrents", "Torrents"])
        .ok_or_else(|| format!("{} has no torrents folder. Is it Transmission's folder?", dir.display()))?;
    let resume = child(dir, &["resume", "Resume"]);
    let mut scan = Scan::default();
    for torrent in files_with(&torrents, "torrent")? {
        // Older versions name the files `<name>.<part of the hash>`, so the name has dots
        // of its own: swap only the last extension.
        let mut name = torrent.file_stem().unwrap_or_default().to_owned();
        name.push(".resume");
        let fields = (resume.as_ref())
            .and_then(|r| std::fs::read(r.join(&name)).ok())
            .and_then(|bytes| top_dict(&bytes))
            .unwrap_or_default();
        scan.found.push(Found {
            torrent,
            save_path: fields.get("destination").and_then(Value::path),
            category: None,
            paused: fields.get("paused").and_then(Value::int).is_some_and(|p| p != 0),
        });
    }
    Ok(scan)
}

/// qBittorrent keeps `<hash>.fastresume` and, unless it was added by a magnet link whose
/// details never arrived, `<hash>.torrent`. The resume file holds `qBt-savePath` (or
/// libtorrent's own `save_path`), `qBt-category`, and `paused` with `auto_managed`.
fn scan_qbittorrent(dir: &Path) -> Result<Scan, String> {
    let resumes = files_with(dir, "fastresume")?;
    if resumes.is_empty() {
        return Err(format!(
            "{} has no .fastresume files. Is it qBittorrent's BT_backup folder?",
            dir.display()
        ));
    }
    let mut scan = Scan::default();
    for resume in resumes {
        let torrent = resume.with_extension("torrent");
        if !torrent.is_file() {
            scan.skipped += 1;
            continue;
        }
        let fields = std::fs::read(&resume)
            .ok()
            .and_then(|b| top_dict(&b))
            .unwrap_or_default();
        let int = |key: &str| fields.get(key).and_then(Value::int);
        let save_path = (fields.get("qBt-savePath").and_then(Value::path))
            .or_else(|| fields.get("save_path").and_then(Value::path));
        let category = (fields.get("qBt-category").and_then(Value::text)).filter(|c| !c.is_empty());
        // Paused by hand: libtorrent's `paused` with `auto_managed` off. A queued torrent is
        // paused but still auto-managed. `qBt-stopped` is a guess at how version 5 says it.
        let paused = (int("paused") == Some(1) && int("auto_managed") != Some(1)) || int("qBt-stopped") == Some(1);
        scan.found.push(Found {
            torrent,
            save_path,
            category,
            paused,
        });
    }
    Ok(scan)
}

/// The first of `names` that is a folder inside `dir`.
fn child(dir: &Path, names: &[&str]) -> Option<PathBuf> {
    names.iter().map(|n| dir.join(n)).find(|p| p.is_dir())
}

/// The files in `dir` ending in `.ext`, sorted.
fn files_with(dir: &Path, ext: &str) -> Result<Vec<PathBuf>, String> {
    let entries = std::fs::read_dir(dir).map_err(|e| format!("could not read {}: {e}", dir.display()))?;
    let mut files: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file() && p.extension().is_some_and(|x| x.eq_ignore_ascii_case(ext)))
        .collect();
    files.sort();
    Ok(files)
}

/// A bencoded value: the format of `.torrent` and resume files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Value {
    Int(i64),
    Bytes(Vec<u8>),
    List(Vec<Value>),
    Dict(BTreeMap<String, Value>),
}

impl Value {
    fn int(&self) -> Option<i64> {
        match self {
            Value::Int(i) => Some(*i),
            _ => None,
        }
    }

    fn text(&self) -> Option<String> {
        match self {
            Value::Bytes(b) => String::from_utf8(b.clone()).ok(),
            _ => None,
        }
    }

    fn path(&self) -> Option<PathBuf> {
        self.text().filter(|t| !t.is_empty()).map(PathBuf::from)
    }
}

/// The keys of a bencoded dictionary. `None` when the bytes are not one.
fn top_dict(bytes: &[u8]) -> Option<BTreeMap<String, Value>> {
    let mut at = 0;
    match parse(bytes, &mut at, 0)? {
        Value::Dict(d) => Some(d),
        _ => None,
    }
}

/// Reads one value starting at `at`. Nesting deeper than 64 is refused, so a made-up
/// file cannot run the stack out.
fn parse(bytes: &[u8], at: &mut usize, depth: u32) -> Option<Value> {
    if depth > 64 {
        return None;
    }
    match *bytes.get(*at)? {
        b'i' => {
            let end = *at + bytes[*at..].iter().position(|&b| b == b'e')?;
            let n = std::str::from_utf8(&bytes[*at + 1..end]).ok()?.parse().ok()?;
            *at = end + 1;
            Some(Value::Int(n))
        }
        b'l' => {
            *at += 1;
            let mut list = Vec::new();
            while *bytes.get(*at)? != b'e' {
                list.push(parse(bytes, at, depth + 1)?);
            }
            *at += 1;
            Some(Value::List(list))
        }
        b'd' => {
            *at += 1;
            let mut dict = BTreeMap::new();
            while *bytes.get(*at)? != b'e' {
                let Value::Bytes(key) = parse(bytes, at, depth + 1)? else {
                    return None;
                };
                let value = parse(bytes, at, depth + 1)?;
                dict.insert(String::from_utf8_lossy(&key).into_owned(), value);
            }
            *at += 1;
            Some(Value::Dict(dict))
        }
        b'0'..=b'9' => {
            let colon = *at + bytes[*at..].iter().position(|&b| b == b':')?;
            let len: usize = std::str::from_utf8(&bytes[*at..colon]).ok()?.parse().ok()?;
            let start = colon + 1;
            let end = start.checked_add(len)?;
            let data = bytes.get(start..end)?.to_vec();
            *at = end;
            Some(Value::Bytes(data))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("rusttorrent-import-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A bencoded dictionary of text and number values, keys in the order given.
    fn dict(pairs: &[(&str, &str)]) -> Vec<u8> {
        let mut out = b"d".to_vec();
        for (k, v) in pairs {
            out.extend(format!("{}:{k}", k.len()).bytes());
            match v.strip_prefix('#') {
                Some(n) => out.extend(format!("i{n}e").bytes()),
                None => out.extend(format!("{}:{v}", v.len()).bytes()),
            }
        }
        out.push(b'e');
        out
    }

    #[test]
    fn reads_bencode() {
        let bytes = b"d1:ai-12e1:bl3:xyzi0ee1:cd1:d0:ee";
        let d = top_dict(bytes).unwrap();
        assert_eq!(d["a"], Value::Int(-12));
        assert_eq!(d["b"], Value::List(vec![Value::Bytes(b"xyz".to_vec()), Value::Int(0)]));
        let Value::Dict(inner) = &d["c"] else { panic!() };
        assert_eq!(inner["d"], Value::Bytes(Vec::new()));
        assert_eq!(top_dict(b"i3e"), None, "not a dictionary");
        assert_eq!(top_dict(b"d1:a"), None, "cut short");
        assert_eq!(top_dict(b"d9999999999999999999:ae"), None, "length past the end");
        let deep = "l".repeat(100) + &"e".repeat(100);
        assert_eq!(parse(deep.as_bytes(), &mut 0, 0), None, "too deep");
    }

    #[test]
    fn transmission() {
        let dir = scratch("tr");
        std::fs::create_dir_all(dir.join("torrents")).unwrap();
        std::fs::create_dir_all(dir.join("resume")).unwrap();
        std::fs::write(dir.join("torrents/A film.1a2b3c.torrent"), b"x").unwrap();
        std::fs::write(dir.join("torrents/bbbb.torrent"), b"x").unwrap();
        std::fs::write(
            dir.join("resume/A film.1a2b3c.resume"),
            dict(&[("destination", "/data/films"), ("paused", "#1")]),
        )
        .unwrap();
        let scan = scan(Client::Transmission, &dir).unwrap();
        assert_eq!(scan.skipped, 0);
        assert_eq!(
            scan.found,
            [
                Found {
                    torrent: dir.join("torrents/A film.1a2b3c.torrent"),
                    save_path: Some(PathBuf::from("/data/films")),
                    category: None,
                    paused: true,
                },
                // No resume file: it goes to the download folder and runs.
                Found {
                    torrent: dir.join("torrents/bbbb.torrent"),
                    save_path: None,
                    category: None,
                    paused: false,
                },
            ]
        );
        assert!(super::scan(Client::Transmission, &dir.join("resume")).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn qbittorrent() {
        let dir = scratch("qb");
        let resume = |hash: &str, pairs: &[(&str, &str)]| {
            std::fs::write(dir.join(format!("{hash}.fastresume")), dict(pairs)).unwrap();
        };
        resume(
            "aa",
            &[
                ("auto_managed", "#0"),
                ("paused", "#1"),
                ("qBt-category", "Linux"),
                ("qBt-savePath", "/d/linux"),
            ],
        );
        // Queued, not stopped; only libtorrent's own key for the folder.
        resume(
            "bb",
            &[("auto_managed", "#1"), ("paused", "#1"), ("save_path", "/d/other")],
        );
        resume("cc", &[("save_path", "/d")]);
        std::fs::write(dir.join("aa.torrent"), b"x").unwrap();
        std::fs::write(dir.join("bb.torrent"), b"x").unwrap();
        let scan = scan(Client::QBittorrent, &dir).unwrap();
        assert_eq!(scan.skipped, 1, "cc is a magnet link with no .torrent file");
        assert_eq!(
            scan.found,
            [
                Found {
                    torrent: dir.join("aa.torrent"),
                    save_path: Some(PathBuf::from("/d/linux")),
                    category: Some("Linux".into()),
                    paused: true,
                },
                Found {
                    torrent: dir.join("bb.torrent"),
                    save_path: Some(PathBuf::from("/d/other")),
                    category: None,
                    paused: false,
                },
            ]
        );
        let empty = scratch("qb-empty");
        assert!(super::scan(Client::QBittorrent, &empty).is_err());
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&empty);
    }
}
