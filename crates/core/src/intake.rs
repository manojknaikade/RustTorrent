//! The one way anything gets added.
//!
//! A clicked magnet link, a double-clicked `.torrent` file, a file dropped on the window,
//! text pasted into the add box and a command-line argument all arrive here as text and
//! leave as a `Source`.
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// A magnet link or an http(s) address of a `.torrent` file.
    Link(String),
    /// A `.torrent` file on this computer.
    File(PathBuf),
}

pub fn parse(text: &str) -> Result<Source, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("Nothing to add.".into());
    }
    let lower = text.to_ascii_lowercase();
    if lower.starts_with("magnet:") {
        if !lower.contains("xt=urn:btih:") && !lower.contains("xt=urn:btmh:") {
            return Err("This magnet link has no torrent identifier (xt=urn:btih:…).".into());
        }
        return Ok(Source::Link(text.to_owned()));
    }
    if lower.starts_with("http://") || lower.starts_with("https://") {
        return Ok(Source::Link(text.to_owned()));
    }
    if lower.starts_with("file://") {
        // file:///Users/me/a%20b.torrent, or file://localhost/Users/…
        let rest = &text[7..];
        let path = match rest.find('/') {
            Some(i) => &rest[i..],
            None => return Err(format!("Not a usable file address: {text}")),
        };
        return Ok(Source::File(PathBuf::from(percent_decode(path))));
    }
    if text.len() == 40 && text.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Ok(Source::Link(format!("magnet:?xt=urn:btih:{text}")));
    }
    if lower.contains("://") {
        return Err(format!("Not a magnet link, web address or torrent file: {text}"));
    }
    Ok(Source::File(PathBuf::from(text)))
}

/// A short name to show while a link's details are still being fetched.
pub fn label(source: &Source) -> String {
    match source {
        Source::File(p) => p
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| p.display().to_string()),
        Source::Link(url) => {
            if let Some(query) = url.strip_prefix("magnet:?").or_else(|| url.strip_prefix("MAGNET:?")) {
                for part in query.split('&') {
                    if let Some(name) = part.strip_prefix("dn=") {
                        if !name.is_empty() {
                            return percent_decode(&name.replace('+', " "));
                        }
                    }
                }
                for part in query.split('&') {
                    if let Some(id) = part.strip_prefix("xt=urn:btih:") {
                        return format!("Magnet link {}", &id[..id.len().min(12)]);
                    }
                }
            }
            url.clone()
        }
    }
}

pub fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            let hex = |c: u8| (c as char).to_digit(16);
            if let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2])) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    const HASH: &str = "0123456789abcdef0123456789abcdef01234567";

    #[test]
    fn magnet_links() {
        let m = format!("magnet:?xt=urn:btih:{HASH}&dn=Some+File%20Name");
        assert_eq!(parse(&format!("  {m}\n")), Ok(Source::Link(m.clone())));
        assert_eq!(label(&Source::Link(m)), "Some File Name");
        assert!(parse("magnet:?dn=no-id").is_err());
    }

    #[test]
    fn bare_hash_becomes_magnet() {
        assert_eq!(parse(HASH), Ok(Source::Link(format!("magnet:?xt=urn:btih:{HASH}"))));
        assert_eq!(label(&parse(HASH).unwrap()), "Magnet link 0123456789ab");
    }

    #[test]
    fn web_addresses() {
        let u = "https://example.org/a.torrent";
        assert_eq!(parse(u), Ok(Source::Link(u.into())));
    }

    #[test]
    fn file_addresses_and_paths() {
        assert_eq!(
            parse("file:///Users/me/My%20Files/a%C3%A9.torrent"),
            Ok(Source::File(PathBuf::from("/Users/me/My Files/aé.torrent")))
        );
        assert_eq!(
            parse("file://localhost/tmp/a.torrent"),
            Ok(Source::File(PathBuf::from("/tmp/a.torrent")))
        );
        assert_eq!(
            parse("/tmp/a b.torrent"),
            Ok(Source::File(PathBuf::from("/tmp/a b.torrent")))
        );
        assert_eq!(label(&Source::File(PathBuf::from("/tmp/a b.torrent"))), "a b.torrent");
    }

    #[test]
    fn rejects_other_things() {
        assert!(parse("").is_err());
        assert!(parse("ftp://example.org/a.torrent").is_err());
    }

    #[test]
    fn percent_decode_edges() {
        assert_eq!(percent_decode("100%"), "100%");
        assert_eq!(percent_decode("%zz%41"), "%zzA");
        assert_eq!(percent_decode("a%2"), "a%2");
        assert_eq!(percent_decode("%41"), "A");
    }
}
