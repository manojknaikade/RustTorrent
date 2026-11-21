//! Making a `.torrent` file from a file or a folder.
//!
//! The engine has a maker of its own, but it cannot report progress or be cancelled,
//! always uses 2 MiB pieces, cannot make a private torrent, lists files in whatever order
//! the disk gives them, and leaves out the last piece when the last file is empty. This
//! one reads in large blocks, hashes with SHA-1 from `crypto-hash` (the system's own SHA-1, which the engine
//! already uses: about five times faster than ring's)
//! and writes the bencode by hand.
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use crypto_hash::{Algorithm, Hasher};

/// The smallest and largest piece sizes offered.
pub const MIN_PIECE: u32 = 16 * 1024;
pub const MAX_PIECE: u32 = 16 * 1024 * 1024;
/// Automatic piece sizes aim for at most this many pieces.
const TARGET_PIECES: u64 = 1500;
const READ_BLOCK: usize = 4 * 1024 * 1024;

/// What the user asked for in the "Create torrent" window.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct MakeOptions {
    /// A file or a folder.
    pub source: PathBuf,
    /// Tracker addresses as typed: one per line. An empty line starts a new tier.
    pub trackers: String,
    /// In bytes. `None` chooses one from the total size.
    pub piece_len: Option<u32>,
    pub private: bool,
    pub comment: String,
    /// Where to write the `.torrent` file.
    pub save_to: PathBuf,
    /// Add the new torrent to the list, pointing at the source, so it seeds.
    pub seed: bool,
}

/// The torrent being made, or the last one made.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct MakeView {
    /// Counts up from 1 with each torrent made.
    pub job: u64,
    pub name: String,
    pub done: u64,
    pub total: u64,
    /// `None` while working. Then the file written, or why it was not.
    pub result: Option<Result<String, String>>,
}

impl MakeView {
    pub fn progress(&self) -> f32 {
        if self.total == 0 {
            0.0
        } else {
            (self.done as f64 / self.total as f64) as f32
        }
    }
}

/// What a torrent file says about itself, for the Overview.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Details {
    pub created_by: Option<String>,
    pub comment: Option<String>,
    /// Seconds since 1970.
    pub created_at: Option<u64>,
    pub piece_len: u32,
    pub pieces: u32,
}

/// Shared between the thread doing the work and whoever watches it.
#[derive(Default, Debug)]
pub struct Progress {
    pub done: AtomicU64,
    pub total: AtomicU64,
    pub cancel: AtomicBool,
}

/// A piece size for this many bytes: a power of two from 16 KiB to 16 MiB, giving at
/// most about 1500 pieces where that is possible.
pub fn auto_piece_len(total: u64) -> u32 {
    let mut len = MIN_PIECE;
    while len < MAX_PIECE && total.div_ceil(len as u64) > TARGET_PIECES {
        len *= 2;
    }
    len
}

struct Entry {
    path: PathBuf,
    /// The path inside the torrent, one part per folder.
    parts: Vec<String>,
    len: u64,
}

fn text_of(name: &std::ffi::OsStr, path: &Path) -> Result<String, String> {
    name.to_str()
        .map(str::to_owned)
        .ok_or_else(|| format!("the name of {} is not valid text", path.display()))
}

/// Every file under `dir`, sorted by path. Links are not followed, and the `.DS_Store`
/// files macOS keeps changing are left out.
fn walk(dir: &Path, parts: &mut Vec<String>, out: &mut Vec<Entry>) -> Result<(), String> {
    let read = std::fs::read_dir(dir).map_err(|e| format!("could not read {}: {e}", dir.display()))?;
    let mut entries: Vec<_> = read
        .collect::<Result<_, _>>()
        .map_err(|e| format!("could not read {}: {e}", dir.display()))?;
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let path = entry.path();
        let name = entry.file_name();
        if name == ".DS_Store" {
            continue;
        }
        let kind = entry
            .file_type()
            .map_err(|e| format!("could not read {}: {e}", path.display()))?;
        if kind.is_dir() {
            parts.push(text_of(&name, &path)?);
            walk(&path, parts, out)?;
            parts.pop();
        } else if kind.is_file() {
            let len = entry
                .metadata()
                .map_err(|e| format!("could not read {}: {e}", path.display()))?
                .len();
            let mut p = parts.clone();
            p.push(text_of(&name, &path)?);
            out.push(Entry { path, parts: p, len });
        }
    }
    Ok(())
}

fn put_bytes(out: &mut Vec<u8>, b: &[u8]) {
    out.extend_from_slice(b.len().to_string().as_bytes());
    out.push(b':');
    out.extend_from_slice(b);
}

fn put_int(out: &mut Vec<u8>, n: u64) {
    out.push(b'i');
    out.extend_from_slice(n.to_string().as_bytes());
    out.push(b'e');
}

/// Tracker tiers from text with one address per line and an empty line between tiers.
fn tiers(text: &str) -> Vec<Vec<String>> {
    let mut tiers = vec![Vec::new()];
    for line in text.lines().map(str::trim) {
        if line.is_empty() {
            if tiers.last().is_some_and(|t| !t.is_empty()) {
                tiers.push(Vec::new());
            }
        } else if !tiers.iter().flatten().any(|t| t == line) {
            tiers.last_mut().unwrap_or_else(|| unreachable!()).push(line.to_owned());
        }
    }
    tiers.retain(|t| !t.is_empty());
    tiers
}

/// A whole torrent file around an info dictionary. Keys are written in sorted order, as
/// bencode requires.
fn wrap(info: &[u8], tiers: &[Vec<String>], comment: &str, created_by: Option<&str>, at: Option<u64>) -> Vec<u8> {
    let mut out = Vec::with_capacity(info.len() + 512);
    out.push(b'd');
    if let Some(first) = tiers.first().and_then(|t| t.first()) {
        put_bytes(&mut out, b"announce");
        put_bytes(&mut out, first.as_bytes());
        if tiers.iter().flatten().count() > 1 {
            put_bytes(&mut out, b"announce-list");
            out.push(b'l');
            for tier in tiers {
                out.push(b'l');
                for url in tier {
                    put_bytes(&mut out, url.as_bytes());
                }
                out.push(b'e');
            }
            out.push(b'e');
        }
    }
    if !comment.is_empty() {
        put_bytes(&mut out, b"comment");
        put_bytes(&mut out, comment.as_bytes());
    }
    if let Some(by) = created_by {
        put_bytes(&mut out, b"created by");
        put_bytes(&mut out, by.as_bytes());
    }
    if let Some(at) = at {
        put_bytes(&mut out, b"creation date");
        put_int(&mut out, at);
    }
    put_bytes(&mut out, b"info");
    out.extend_from_slice(info);
    out.push(b'e');
    out
}

/// A torrent file made from a torrent's info dictionary and its trackers, for a torrent
/// whose original file the engine did not keep.
pub fn from_info(info: &[u8], trackers: &[String]) -> Vec<u8> {
    let tiers: Vec<Vec<String>> = trackers.iter().map(|t| vec![t.clone()]).collect();
    wrap(info, &tiers, "", None, None)
}

/// Reads every file once and returns the bytes of the torrent file. Checks `cancel`
/// between blocks and adds what it read to `done`.
pub fn build(opts: &MakeOptions, progress: &Progress) -> Result<Vec<u8>, String> {
    let source = &opts.source;
    let meta = std::fs::metadata(source).map_err(|e| format!("could not read {}: {e}", source.display()))?;
    let name = match source.file_name() {
        Some(n) => text_of(n, source)?,
        None => return Err(format!("{} cannot be shared as a torrent", source.display())),
    };
    let single = !meta.is_dir();
    let files = if single {
        vec![Entry {
            path: source.clone(),
            parts: Vec::new(),
            len: meta.len(),
        }]
    } else {
        let mut files = Vec::new();
        walk(source, &mut Vec::new(), &mut files)?;
        if files.is_empty() {
            return Err("there are no files in it".into());
        }
        files
    };
    let total: u64 = files.iter().map(|f| f.len).sum();
    if total == 0 {
        return Err("it has nothing in it: every file is empty".into());
    }
    progress.total.store(total, Ordering::Relaxed);
    let piece_len = opts
        .piece_len
        .unwrap_or_else(|| auto_piece_len(total))
        .clamp(MIN_PIECE, MAX_PIECE);

    // Pieces run on from one file into the next.
    let mut pieces = Vec::with_capacity(total.div_ceil(piece_len as u64) as usize * 20);
    let mut ctx = Hasher::new(Algorithm::SHA1);
    let mut in_piece = 0usize;
    let mut buf = vec![0u8; READ_BLOCK];
    for f in &files {
        let mut file = File::open(&f.path).map_err(|e| format!("could not open {}: {e}", f.path.display()))?;
        let mut read = 0u64;
        loop {
            if progress.cancel.load(Ordering::Relaxed) {
                return Err("cancelled".into());
            }
            let n = match file.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(format!("could not read {}: {e}", f.path.display())),
            };
            read += n as u64;
            if read > f.len {
                break;
            }
            let mut block = &buf[..n];
            while !block.is_empty() {
                let take = (piece_len as usize - in_piece).min(block.len());
                ctx.write_all(&block[..take])
                    .map_err(|e| format!("could not hash: {e}"))?;
                in_piece += take;
                block = &block[take..];
                if in_piece == piece_len as usize {
                    let mut done = std::mem::replace(&mut ctx, Hasher::new(Algorithm::SHA1));
                    pieces.extend_from_slice(&done.finish());
                    in_piece = 0;
                }
            }
            progress.done.fetch_add(n as u64, Ordering::Relaxed);
        }
        if read != f.len {
            return Err(format!("{} changed while it was being read", f.path.display()));
        }
    }
    if in_piece > 0 {
        pieces.extend_from_slice(&ctx.finish());
    }

    // The info dictionary, keys in sorted order.
    let mut info = Vec::with_capacity(pieces.len() + files.len() * 64 + 128);
    info.push(b'd');
    if single {
        put_bytes(&mut info, b"length");
        put_int(&mut info, total);
    } else {
        put_bytes(&mut info, b"files");
        info.push(b'l');
        for f in &files {
            info.push(b'd');
            put_bytes(&mut info, b"length");
            put_int(&mut info, f.len);
            put_bytes(&mut info, b"path");
            info.push(b'l');
            for part in &f.parts {
                put_bytes(&mut info, part.as_bytes());
            }
            info.push(b'e');
            info.push(b'e');
        }
        info.push(b'e');
    }
    put_bytes(&mut info, b"name");
    put_bytes(&mut info, name.as_bytes());
    put_bytes(&mut info, b"piece length");
    put_int(&mut info, piece_len as u64);
    put_bytes(&mut info, b"pieces");
    put_bytes(&mut info, &pieces);
    if opts.private {
        put_bytes(&mut info, b"private");
        put_int(&mut info, 1);
    }
    info.push(b'e');

    let by = format!("RustTorrent {}", crate::VERSION);
    Ok(wrap(
        &info,
        &tiers(&opts.trackers),
        opts.comment.trim(),
        Some(&by),
        Some(crate::now_secs()),
    ))
}

/// Builds the torrent and writes it where the user asked.
pub fn make(opts: &MakeOptions, progress: &Progress) -> Result<Vec<u8>, String> {
    let bytes = build(opts, progress)?;
    std::fs::write(&opts.save_to, &bytes).map_err(|e| format!("could not write {}: {e}", opts.save_to.display()))?;
    Ok(bytes)
}

/// What the Overview shows, read from a torrent file.
pub fn details(torrent: &[u8]) -> Option<Details> {
    let t = librqbit::torrent_from_bytes(torrent).ok()?;
    let text = |b: &Option<librqbit::ByteBuf<'_>>| {
        b.as_ref()
            .map(|b| String::from_utf8_lossy(b.as_ref()).trim().to_owned())
            .filter(|s| !s.is_empty())
    };
    let info = &t.info.data;
    Some(Details {
        created_by: text(&t.created_by),
        comment: text(&t.comment),
        created_at: t.creation_date.map(|d| d as u64).filter(|&d| d > 0),
        piece_len: info.piece_length,
        pieces: (info.pieces.as_ref().len() / 20) as u32,
    })
}

/// A date in local time, such as "3 Nov 2026, 14:05".
pub fn date_text(secs: u64) -> String {
    chrono::DateTime::from_timestamp(secs as i64, 0)
        .map(|d| d.with_timezone(&chrono::Local).format("%-d %b %Y, %H:%M").to_string())
        .unwrap_or_else(|| "–".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sha1(b: &[u8]) -> Vec<u8> {
        crypto_hash::digest(Algorithm::SHA1, b)
    }

    /// A fresh folder under the system's temporary folder.
    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("rusttorrent-make-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Bytes that differ from file to file and from place to place.
    fn data(seed: u8, len: usize) -> Vec<u8> {
        (0..len).map(|i| (i as u32 * 31 + seed as u32 * 7) as u8).collect()
    }

    /// The info dictionary's raw bytes: everything after `4:info` but the final `e`.
    fn info_slice(torrent: &[u8]) -> &[u8] {
        let at = torrent.windows(6).position(|w| w == b"4:info").unwrap();
        &torrent[at + 6..torrent.len() - 1]
    }

    #[test]
    fn piece_sizes() {
        assert_eq!(auto_piece_len(1), MIN_PIECE);
        assert_eq!(auto_piece_len(1500 * 16 * 1024), MIN_PIECE);
        assert_eq!(auto_piece_len(1500 * 16 * 1024 + 1), 32 * 1024);
        assert_eq!(auto_piece_len(700 << 20), 512 * 1024);
        assert_eq!(auto_piece_len(4 << 30), 4 << 20);
        assert_eq!(auto_piece_len(1 << 40), MAX_PIECE);
    }

    #[test]
    fn folder_with_odd_and_empty_files() {
        let dir = scratch("folder");
        let src = dir.join("My Folder");
        let piece = MIN_PIECE as usize;
        // Sorted order: a.txt, b/one-piece.bin, c/empty, d.bin, z-empty. The last file is
        // empty and the total is not a whole number of pieces.
        let files: Vec<(&str, Vec<u8>)> = vec![
            ("d.bin", data(4, 40_000)),
            ("a.txt", data(1, 5_000)),
            ("b/one-piece.bin", data(2, piece)),
            ("c/empty", Vec::new()),
            ("z-empty", Vec::new()),
        ];
        for (name, bytes) in &files {
            let path = src.join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, bytes).unwrap();
        }
        std::fs::write(src.join(".DS_Store"), b"left out").unwrap();
        let opts = MakeOptions {
            source: src.clone(),
            trackers: "udp://one.example:80/announce\nhttp://two.example/announce\n\nudp://three.example:6969".into(),
            piece_len: Some(MIN_PIECE),
            private: true,
            comment: "Test folder".into(),
            save_to: dir.join("out.torrent"),
            seed: false,
        };
        let progress = Progress::default();
        let torrent = make(&opts, &progress).unwrap();
        assert_eq!(std::fs::read(&opts.save_to).unwrap(), torrent);

        let mut all = Vec::new();
        for name in ["a.txt", "b/one-piece.bin", "c/empty", "d.bin", "z-empty"] {
            all.extend_from_slice(&files.iter().find(|f| f.0 == name).unwrap().1);
        }
        assert_eq!(progress.total.load(Ordering::Relaxed), all.len() as u64);
        assert_eq!(progress.done.load(Ordering::Relaxed), all.len() as u64);

        // Read back by the engine's own parser, which hashes with its own SHA-1.
        let t = librqbit::torrent_from_bytes(&torrent).unwrap();
        assert_eq!(t.info_hash.0.to_vec(), sha1(info_slice(&torrent)));
        assert_eq!(t.info.raw_bytes.as_ref(), info_slice(&torrent));
        let info = &t.info.data;
        assert_eq!(info.name.as_ref().unwrap().as_ref(), b"My Folder");
        assert_eq!(info.piece_length, MIN_PIECE);
        assert!(info.private);
        let expected: Vec<u8> = all.chunks(piece).flat_map(sha1).collect();
        assert_eq!(all.len().div_ceil(piece), 4);
        assert_eq!(info.pieces.as_ref(), expected.as_slice());
        let listed: Vec<(String, u64)> = info
            .clone()
            .validate()
            .unwrap()
            .iter_file_details()
            .map(|f| (f.filename.to_vec().join("/"), f.len))
            .collect();
        assert_eq!(
            listed,
            vec![
                ("a.txt".to_owned(), 5_000),
                ("b/one-piece.bin".to_owned(), piece as u64),
                ("c/empty".to_owned(), 0),
                ("d.bin".to_owned(), 40_000),
                ("z-empty".to_owned(), 0),
            ]
        );
        let tiers: Vec<Vec<&[u8]>> = t
            .announce_list
            .iter()
            .map(|tier| tier.iter().map(|u| u.as_ref()).collect())
            .collect();
        assert_eq!(
            tiers,
            vec![
                vec![&b"udp://one.example:80/announce"[..], b"http://two.example/announce"],
                vec![b"udp://three.example:6969"],
            ]
        );
        assert_eq!(t.announce.as_ref().unwrap().as_ref(), b"udp://one.example:80/announce");

        let d = details(&torrent).unwrap();
        assert_eq!(d.comment.as_deref(), Some("Test folder"));
        assert_eq!(d.created_by, Some(format!("RustTorrent {}", crate::VERSION)));
        assert!(d.created_at.is_some_and(|at| at > 1_700_000_000));
        assert_eq!((d.piece_len, d.pieces), (MIN_PIECE, 4));

        // The same info wrapped again keeps the same info-hash.
        let again = from_info(info_slice(&torrent), &["udp://one.example:80/announce".to_owned()]);
        assert_eq!(librqbit::torrent_from_bytes(&again).unwrap().info_hash, t.info_hash);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn one_file_exactly_one_piece() {
        let dir = scratch("single");
        let bytes = data(9, MIN_PIECE as usize);
        std::fs::write(dir.join("one.bin"), &bytes).unwrap();
        let opts = MakeOptions {
            source: dir.join("one.bin"),
            ..Default::default()
        };
        let torrent = build(&opts, &Progress::default()).unwrap();
        let t = librqbit::torrent_from_bytes(&torrent).unwrap();
        assert_eq!(t.info_hash.0.to_vec(), sha1(info_slice(&torrent)));
        let info = &t.info.data;
        assert_eq!(info.length, Some(MIN_PIECE as u64));
        assert!(info.files.is_none());
        assert!(!info.private);
        assert!(t.announce.is_none() && t.comment.is_none());
        assert_eq!(info.pieces.as_ref(), sha1(&bytes).as_slice());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn refuses_empty_and_stops_when_cancelled() {
        let dir = scratch("empty");
        std::fs::create_dir_all(dir.join("src")).unwrap();
        let mut opts = MakeOptions {
            source: dir.join("src"),
            ..Default::default()
        };
        assert!(build(&opts, &Progress::default()).is_err());
        std::fs::write(dir.join("src/empty"), b"").unwrap();
        assert!(build(&opts, &Progress::default()).is_err());
        std::fs::write(dir.join("src/full"), data(3, 1000)).unwrap();
        let progress = Progress::default();
        progress.cancel.store(true, Ordering::Relaxed);
        assert_eq!(build(&opts, &progress), Err("cancelled".to_owned()));
        opts.source = dir.join("missing");
        assert!(build(&opts, &Progress::default()).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn tracker_tiers() {
        assert!(tiers("\n  \n").is_empty());
        assert_eq!(
            tiers("a\n\n\nb\nc\na\n"),
            vec![vec!["a".to_owned()], vec!["b".to_owned(), "c".to_owned()]]
        );
    }
}
