//! RSS and Atom feeds: reading a feed's items and deciding which ones to download.
//!
//! Nothing here touches the network. The reader is deliberately forgiving: feeds from
//! torrent sites are often not valid XML, so it looks for the few elements it needs
//! instead of parsing the whole document.
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(default)]
pub struct Feed {
    pub name: String,
    pub url: String,
    /// Words that must all appear in an item's title. Empty means every item.
    pub include: String,
    /// Words of which none may appear in an item's title.
    pub exclude: String,
    /// The category given to torrents added from this feed.
    pub category: Option<String>,
    pub enabled: bool,
}

impl Default for Feed {
    fn default() -> Self {
        Self {
            name: String::new(),
            url: String::new(),
            include: String::new(),
            exclude: String::new(),
            category: None,
            enabled: true,
        }
    }
}

impl Feed {
    /// Whether an item with this title should be downloaded. Case does not matter.
    pub fn wants(&self, title: &str) -> bool {
        let title = title.to_lowercase();
        let has = |word: &str| title.contains(&word.to_lowercase());
        self.include.split_whitespace().all(has) && !self.exclude.split_whitespace().any(has)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item {
    /// What tells this item apart from the others in the feed.
    pub id: String,
    pub title: String,
    /// A magnet link or the address of a `.torrent` file.
    pub link: String,
}

/// The items of a feed, in the order they appear. Items without a link are left out.
pub fn parse(xml: &str) -> Vec<Item> {
    let lower = xml.to_ascii_lowercase();
    let mut items = Vec::new();
    let mut at = 0;
    while let Some((start, name)) = next_block(&lower, at) {
        let close = format!("</{name}>");
        let end = lower[start..].find(&close).map_or(lower.len(), |i| start + i);
        let (block, block_lower) = (&xml[start..end], &lower[start..end]);
        at = (end + close.len()).min(lower.len());

        let title = text_of(block, block_lower, "title").unwrap_or_default();
        let link = attr_of(block, block_lower, "enclosure", "url")
            .or_else(|| text_of(block, block_lower, "torrent:magneturi"))
            .or_else(|| text_of(block, block_lower, "link"))
            .or_else(|| attr_of(block, block_lower, "link", "href"))
            .unwrap_or_default();
        if link.is_empty() {
            continue;
        }
        let id = text_of(block, block_lower, "guid")
            .or_else(|| text_of(block, block_lower, "id"))
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| link.clone());
        items.push(Item { id, title, link });
    }
    items
}

/// The start of the next `<item>` or `<entry>` element at or after `from`.
fn next_block(lower: &str, from: usize) -> Option<(usize, &'static str)> {
    ["item", "entry"]
        .into_iter()
        .filter_map(|name| open_tag(lower, from, name).map(|i| (i, name)))
        .min()
}

/// Where `<name` starts as a whole tag name, at or after `from`.
fn open_tag(lower: &str, from: usize, name: &str) -> Option<usize> {
    let needle = format!("<{name}");
    let mut at = from;
    while let Some(i) = lower.get(at..)?.find(&needle) {
        let start = at + i;
        match lower.as_bytes().get(start + needle.len()) {
            Some(b'>' | b'/' | b' ' | b'\t' | b'\r' | b'\n') => return Some(start),
            _ => at = start + needle.len(),
        }
    }
    None
}

/// The text inside the first `<name>…</name>`.
fn text_of(block: &str, lower: &str, name: &str) -> Option<String> {
    let start = open_tag(lower, 0, name)?;
    let tag_end = start + lower[start..].find('>')?;
    if lower.as_bytes()[tag_end - 1] == b'/' {
        return None;
    }
    let body = tag_end + 1;
    let end = body + lower[body..].find(&format!("</{name}>"))?;
    let inner = block[body..end].trim();
    let text = match inner.strip_prefix("<![CDATA[").and_then(|s| s.strip_suffix("]]>")) {
        Some(raw) => raw.trim().to_owned(),
        None => unescape(inner),
    };
    Some(text)
}

/// The value of an attribute of the first `<tag …>`.
fn attr_of(block: &str, lower: &str, tag: &str, attr: &str) -> Option<String> {
    let mut from = 0;
    loop {
        let start = open_tag(lower, from, tag)?;
        let end = start + lower[start..].find('>')?;
        for quote in ['"', '\''] {
            let needle = format!("{attr}={quote}");
            if let Some(i) = lower[start..end].find(&needle) {
                let value = start + i + needle.len();
                let len = block[value..end].find(quote)?;
                return Some(unescape(block[value..value + len].trim()));
            }
        }
        from = end;
    }
}

fn unescape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        let known = rest.find(';').filter(|&end| end <= 10).and_then(|end| {
            let name = &rest[1..end];
            let ch = match name {
                "amp" => Some('&'),
                "lt" => Some('<'),
                "gt" => Some('>'),
                "quot" => Some('"'),
                "apos" => Some('\''),
                _ => {
                    let code = match name.strip_prefix("#x").or_else(|| name.strip_prefix("#X")) {
                        Some(hex) => u32::from_str_radix(hex, 16).ok(),
                        None => name.strip_prefix('#').and_then(|n| n.parse().ok()),
                    };
                    code.and_then(char::from_u32)
                }
            };
            ch.map(|c| (c, end))
        });
        match known {
            Some((c, end)) => {
                out.push(c);
                rest = &rest[end + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_rss_items() {
        let xml = r#"<?xml version="1.0"?>
<rss version="2.0" xmlns:torrent="http://xmlns.ezrss.it/0.1/"><channel>
  <title>Feed title, not an item</title>
  <link>https://example.org/</link>
  <item>
    <title>Debian 13.7 &amp; friends</title>
    <link>https://example.org/page/1</link>
    <guid isPermaLink="false">id-1</guid>
    <enclosure url="https://example.org/a.torrent?x=1&amp;y=2" type="application/x-bittorrent" length="10"/>
  </item>
  <item>
    <title><![CDATA[Ubuntu <25.10> desktop]]></title>
    <torrent:magnetURI><![CDATA[magnet:?xt=urn:btih:abc&dn=u]]></torrent:magnetURI>
    <link>https://example.org/page/2</link>
  </item>
  <item>
    <title>Only a link</title>
    <link>https://example.org/c.torrent</link>
  </item>
  <item><title>No link at all</title></item>
</channel></rss>"#;
        let items = parse(xml);
        assert_eq!(items.len(), 3);
        assert_eq!(items[0].title, "Debian 13.7 & friends");
        assert_eq!(items[0].id, "id-1");
        assert_eq!(items[0].link, "https://example.org/a.torrent?x=1&y=2");
        assert_eq!(items[1].title, "Ubuntu <25.10> desktop");
        assert_eq!(items[1].link, "magnet:?xt=urn:btih:abc&dn=u");
        assert_eq!(items[1].id, items[1].link);
        assert_eq!(items[2].link, "https://example.org/c.torrent");
    }

    #[test]
    fn reads_atom_entries() {
        let xml = r#"<feed xmlns="http://www.w3.org/2005/Atom">
  <title>Releases</title>
  <link href="https://example.org/"/>
  <entry>
    <id>tag:example.org,2026:1</id>
    <title type="html">Release &#49;&#x2E;0</title>
    <link rel="alternate" href='https://example.org/r1.torrent'/>
  </entry>
</feed>"#;
        let items = parse(xml);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].id, "tag:example.org,2026:1");
        assert_eq!(items[0].title, "Release 1.0");
        assert_eq!(items[0].link, "https://example.org/r1.torrent");
    }

    #[test]
    fn survives_rubbish() {
        assert!(parse("").is_empty());
        assert!(parse("<html><body>Not found</body></html>").is_empty());
        assert!(parse("<rss><item><title>cut off").is_empty());
        assert!(parse("<items><itemize>x</itemize></items>").is_empty());
        let cut = parse("<item><link>magnet:?xt=urn:btih:abc</link><title>é &bogus; & more");
        assert_eq!(cut.len(), 1);
        assert_eq!(cut[0].title, "");
    }

    #[test]
    fn filter_words() {
        let feed = |include: &str, exclude: &str| Feed {
            include: include.into(),
            exclude: exclude.into(),
            ..Default::default()
        };
        assert!(feed("", "").wants("anything"));
        assert!(feed("debian ARM64", "").wants("Debian 13.7 arm64 netinst"));
        assert!(!feed("debian arm64", "").wants("Debian 13.7 amd64 netinst"));
        assert!(!feed("debian", "netinst dvd").wants("Debian 13.7 arm64 NETINST"));
        assert!(feed("", "dvd").wants("Debian 13.7 arm64 netinst"));
    }
}
