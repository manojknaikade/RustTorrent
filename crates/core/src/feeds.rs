//! RSS and Atom feeds: reading a feed's items and deciding which ones to download.
//!
//! Nothing here touches the network. The reader is deliberately forgiving: feeds from
//! torrent sites are often not valid XML, so it looks for the few elements it needs
//! instead of parsing the whole document.
use serde::{Deserialize, Serialize};

use crate::episodes::{self, Filter};

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
    /// `include` and `exclude` are regular expressions instead of words.
    pub regex: bool,
    /// Which episodes to take, such as "S02E05-". Empty means any. See [`crate::episodes`].
    pub episodes: String,
    /// Take each season and episode once, unless the title says REPACK or PROPER.
    pub skip_repeats: bool,
    /// When the feed is first read, skip everything it already lists.
    pub newer_only: bool,
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
            regex: false,
            episodes: String::new(),
            skip_repeats: false,
            newer_only: false,
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

/// Longest regular expression accepted, in characters.
pub const MAX_PATTERN: usize = 500;
/// Only this much of a title is tried against a regular expression.
const MAX_TITLE: usize = 1000;

fn compile(pattern: &str, what: &str) -> Result<Option<regex_lite::Regex>, String> {
    let pattern = pattern.trim();
    if pattern.is_empty() {
        return Ok(None);
    }
    if pattern.chars().count() > MAX_PATTERN {
        return Err(format!(
            "The “{what}” expression is too long. At most {MAX_PATTERN} characters."
        ));
    }
    // regex-lite never backtracks, so a search takes time in proportion to the title;
    // the limits keep the expression itself small.
    regex_lite::RegexBuilder::new(pattern)
        .case_insensitive(true)
        .size_limit(256 * 1024)
        .nest_limit(50)
        .build()
        .map(Some)
        .map_err(|e| {
            let why = e.to_string();
            let why = why.lines().map(str::trim).rfind(|l| !l.is_empty()).unwrap_or("");
            format!("The “{what}” expression is not valid: {why}")
        })
}

enum Words {
    Plain,
    Regex {
        include: Option<regex_lite::Regex>,
        exclude: Option<regex_lite::Regex>,
    },
}

/// A feed's filter, checked and ready to try titles against.
pub struct Rule {
    feed: Feed,
    words: Words,
    episodes: Filter,
}

/// What a rule makes of one item.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Download it.
    Take,
    /// The title does not pass the filter.
    NoMatch,
    /// This item was handed to the downloader before.
    Seen,
    /// The same season and episode was downloaded before.
    Repeat,
}

impl Feed {
    /// The feed's filter, or a message saying what is wrong with it.
    pub fn rule(&self) -> Result<Rule, String> {
        let words = if self.regex {
            Words::Regex {
                include: compile(&self.include, "Must have")?,
                exclude: compile(&self.exclude, "Must not have")?,
            }
        } else {
            Words::Plain
        };
        Ok(Rule {
            feed: self.clone(),
            words,
            episodes: Filter::parse(&self.episodes)?,
        })
    }
}

impl Rule {
    /// Whether the title passes the words or expressions and the episode filter.
    pub fn wants(&self, title: &str) -> bool {
        let words = match &self.words {
            Words::Plain => self.feed.wants(title),
            Words::Regex { include, exclude } => {
                let end = title.char_indices().nth(MAX_TITLE).map_or(title.len(), |(i, _)| i);
                let title = &title[..end];
                include.as_ref().is_none_or(|r| r.is_match(title))
                    && !exclude.as_ref().is_some_and(|r| r.is_match(title))
            }
        };
        words && self.episodes.wants_title(title)
    }

    /// The text remembered for an item once it is taken, so a repeat of it can be
    /// skipped. `None` when repeats are not skipped or the title has no episode.
    pub fn episode_key(&self, title: &str) -> Option<String> {
        self.feed
            .skip_repeats
            .then(|| episodes::find(title))
            .flatten()
            .map(|e| e.key())
    }

    /// What to do with an item, given the items and episodes already taken.
    pub fn judge(&self, item: &Item, seen: &[String], taken: &[String]) -> Verdict {
        if seen.contains(&item.id) {
            return Verdict::Seen;
        }
        if !self.wants(&item.title) {
            return Verdict::NoMatch;
        }
        match self.episode_key(&item.title) {
            Some(key) if taken.contains(&key) && !episodes::is_fix(&item.title) => Verdict::Repeat,
            _ => Verdict::Take,
        }
    }

    /// True when a read of the feed only marks what it lists as old: it takes only newer
    /// items and has not been read since that was turned on.
    pub fn first_read(&self, primed: bool) -> bool {
        self.feed.newer_only && !primed
    }

    /// The items to download from one read of the feed, oldest first. `items` is in the
    /// feed's order, newest first. `seen` and `taken` are brought up to date.
    ///
    /// When the feed takes only newer items and has not been read before (`primed` is
    /// false), everything it lists now is marked as seen and nothing is taken.
    pub fn pick(&self, items: Vec<Item>, primed: bool, seen: &mut Vec<String>, taken: &mut Vec<String>) -> Vec<Item> {
        if self.first_read(primed) {
            for item in items.into_iter().rev() {
                if !seen.contains(&item.id) {
                    seen.push(item.id);
                }
            }
            return Vec::new();
        }
        let mut out = Vec::new();
        for item in items.into_iter().rev() {
            match self.judge(&item, seen, taken) {
                Verdict::Take => {
                    if let Some(key) = self.episode_key(&item.title) {
                        if !taken.contains(&key) {
                            taken.push(key);
                        }
                    }
                    seen.push(item.id.clone());
                    out.push(item);
                }
                // A repeat is not marked as seen: if it was skipped by mistake, turning
                // the option off brings it back.
                Verdict::NoMatch | Verdict::Seen | Verdict::Repeat => {}
            }
        }
        out
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

    fn item(id: &str, title: &str) -> Item {
        Item {
            id: id.into(),
            title: title.into(),
            link: format!("magnet:?xt=urn:btih:{id}"),
        }
    }

    fn titles(items: &[Item]) -> Vec<&str> {
        items.iter().map(|i| i.title.as_str()).collect()
    }

    #[test]
    fn filter_regex() {
        let feed = |include: &str, exclude: &str| Feed {
            include: include.into(),
            exclude: exclude.into(),
            regex: true,
            ..Default::default()
        };
        let rule = feed(r"^debian 1[3-9]\.\d+ (arm64|amd64)", "dvd").rule().unwrap();
        assert!(rule.wants("Debian 13.7 ARM64 netinst"));
        assert!(rule.wants("debian 14.0 amd64 netinst"));
        assert!(!rule.wants("Debian 12.9 amd64 netinst"));
        assert!(!rule.wants("Debian 13.7 amd64 DVD-1"));
        assert!(!rule.wants("Ubuntu Debian 13.7 amd64"));
        assert!(feed("", "").rule().unwrap().wants("anything"));
        // The same text as words means something else.
        let words = Feed {
            regex: false,
            ..feed("debian.13", "")
        };
        assert!(!words.rule().unwrap().wants("Debian 13"));
    }

    #[test]
    fn bad_regex_says_why() {
        let feed = |include: &str, exclude: &str| Feed {
            include: include.into(),
            exclude: exclude.into(),
            regex: true,
            ..Default::default()
        };
        for bad in ["(unclosed", "[z-a]", "a{5}{5}{5}{5}{5}{5}{5}", "*", "(?<", r"\"] {
            let e = feed(bad, "")
                .rule()
                .err()
                .unwrap_or_else(|| panic!("{bad} was accepted"));
            assert!(e.starts_with("The “Must have” expression"), "{e}");
        }
        let e = feed("", "(").rule().err().unwrap();
        assert!(e.starts_with("The “Must not have” expression is not valid"), "{e}");
        let e = feed(&"a".repeat(MAX_PATTERN + 1), "").rule().err().unwrap();
        assert!(e.contains("too long"), "{e}");
        assert!(feed(&"a".repeat(MAX_PATTERN), "").rule().is_ok());
        // Words are never read as expressions, so "(" is fine there.
        let words = Feed {
            include: "(".into(),
            ..Default::default()
        };
        assert!(words.rule().unwrap().wants("a ( b"));
    }

    #[test]
    fn regex_does_not_hang() {
        // Takes forever with a backtracking engine.
        let rule = Feed {
            include: "(a+)+$".into(),
            regex: true,
            ..Default::default()
        }
        .rule()
        .unwrap();
        let title = format!("{}b", "a".repeat(MAX_TITLE - 1));
        let start = std::time::Instant::now();
        assert!(!rule.wants(&title));
        assert!(start.elapsed() < std::time::Duration::from_secs(2));
    }

    #[test]
    fn bad_episode_filter_says_why() {
        let feed = Feed {
            episodes: "S02E05-S01".into(),
            ..Default::default()
        };
        assert!(feed.rule().err().unwrap().contains("ends before"));
    }

    #[test]
    fn episodes_and_words_together() {
        let rule = Feed {
            include: "show 1080p".into(),
            episodes: "S02E05-".into(),
            ..Default::default()
        }
        .rule()
        .unwrap();
        assert!(rule.wants("Show.S02E05.1080p"));
        assert!(!rule.wants("Show.S02E05.720p"));
        assert!(!rule.wants("Show.S02E04.1080p"));
        assert!(!rule.wants("Show.Special.1080p"));
    }

    #[test]
    fn repeats_are_taken_once() {
        let rule = Feed {
            skip_repeats: true,
            ..Default::default()
        }
        .rule()
        .unwrap();
        // Newest first, as feeds list them.
        let items = vec![
            item("5", "Show S02E06 720p"),
            item("4", "Show S02E05 REPACK 1080p"),
            item("3", "Show 2x05 720p"),
            item("2", "Show S02E05 1080p"),
            item("1", "Show Special"),
        ];
        let (mut seen, mut taken) = (Vec::new(), Vec::new());
        let got = rule.pick(items.clone(), true, &mut seen, &mut taken);
        assert_eq!(
            titles(&got),
            [
                "Show Special",
                "Show S02E05 1080p",
                "Show S02E05 REPACK 1080p",
                "Show S02E06 720p"
            ]
        );
        assert_eq!(taken, ["S02E05", "S02E06"]);
        assert_eq!(rule.judge(&items[2], &seen, &taken), Verdict::Repeat);
        assert_eq!(rule.judge(&items[0], &seen, &taken), Verdict::Seen);
        // A later read takes nothing again.
        assert!(rule.pick(items, true, &mut seen, &mut taken).is_empty());
        // The same episode under a new id is still a repeat.
        let again = vec![item("6", "Show.S02E06.1080p"), item("7", "Show S02E07")];
        assert_eq!(titles(&rule.pick(again, true, &mut seen, &mut taken)), ["Show S02E07"]);
    }

    #[test]
    fn repeats_are_taken_when_the_option_is_off() {
        let rule = Feed::default().rule().unwrap();
        let items = vec![item("2", "Show S02E05 720p"), item("1", "Show S02E05 1080p")];
        let (mut seen, mut taken) = (Vec::new(), Vec::new());
        assert_eq!(rule.pick(items, true, &mut seen, &mut taken).len(), 2);
        assert!(taken.is_empty());
    }

    #[test]
    fn newer_only_skips_what_is_listed_first() {
        let rule = Feed {
            newer_only: true,
            ..Default::default()
        }
        .rule()
        .unwrap();
        let first = vec![item("2", "Two"), item("1", "One")];
        let (mut seen, mut taken) = (Vec::new(), Vec::new());
        assert!(rule.pick(first.clone(), false, &mut seen, &mut taken).is_empty());
        assert_eq!(seen, ["1", "2"]);
        // The next read, primed now, takes only what is new.
        let mut next = vec![item("4", "Four"), item("3", "Three")];
        next.extend(first);
        assert_eq!(titles(&rule.pick(next, true, &mut seen, &mut taken)), ["Three", "Four"]);
        // Without the option the first read takes everything.
        let all = Feed::default().rule().unwrap();
        let (mut seen, mut taken) = (Vec::new(), Vec::new());
        assert_eq!(all.pick(vec![item("1", "One")], false, &mut seen, &mut taken).len(), 1);
    }

    #[test]
    fn old_feeds_read_with_new_fields_off() {
        let feed: Feed = serde_json::from_str(r#"{"name":"a","url":"https://x/","include":"b"}"#).unwrap();
        assert!(!feed.regex && !feed.skip_repeats && !feed.newer_only && feed.enabled);
        assert!(feed.episodes.is_empty());
    }
}
