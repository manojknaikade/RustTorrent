//! Season and episode numbers in item titles, and filters such as "S02E05-".
//!
//! Pure logic: nothing here touches the network or the disk.
//!
//! A title is read as an episode when it has one of these, in any case:
//! `S02E05`, `S2E5`, `S02.E05`, `S02 E05`, `2x05`, or `Season 2 Episode 5`.
//! `1920x1080` and `x264` are not episodes: `NxM` needs a season of one or two digits
//! standing on its own.
//!
//! A filter is a list of parts separated by commas or semicolons. A title matches when
//! any part matches:
//!
//! | Part            | Means                                      |
//! |-----------------|--------------------------------------------|
//! | `S02`           | every episode of season 2                  |
//! | `S02-`          | season 2 and every later season            |
//! | `S02E05`        | that one episode                           |
//! | `S02E05-`       | that episode and every later one           |
//! | `S02E05-S03E02` | everything from the first to the second    |
//! | `S02E05-10`     | episodes 5 to 10 of season 2               |
//!
//! `2x05` can be written wherever `S02E05` can.

/// One episode. Ordered by season, then episode.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Episode {
    pub season: u32,
    pub episode: u32,
}

impl Episode {
    /// The same text for every way of writing it, for remembering what was downloaded.
    pub fn key(self) -> String {
        format!("S{:02}E{:02}", self.season, self.episode)
    }
}

/// Longest number taken as a season or an episode. Keeps out years and resolutions.
const MAX_DIGITS: usize = 4;

fn digits(b: &[u8], at: usize) -> Option<(u32, usize)> {
    let end = b[at..]
        .iter()
        .position(|c| !c.is_ascii_digit())
        .map_or(b.len(), |n| at + n);
    if end == at || end - at > MAX_DIGITS {
        return None;
    }
    let n = std::str::from_utf8(&b[at..end]).ok()?.parse().ok()?;
    Some((n, end))
}

fn word_start(b: &[u8], at: usize) -> bool {
    at == 0 || !b[at - 1].is_ascii_alphanumeric()
}

fn word_end(b: &[u8], at: usize) -> bool {
    at >= b.len() || !b[at].is_ascii_alphanumeric()
}

/// Skips spaces, dots, dashes, underscores and commas.
fn skip_gap(b: &[u8], mut at: usize) -> usize {
    while at < b.len() && matches!(b[at], b' ' | b'.' | b'-' | b'_' | b',') {
        at += 1;
    }
    at
}

/// `s02e05`, `s2e5`, `s02.e05`, `s02 e05`, at `at`.
fn sxxexx(b: &[u8], at: usize) -> Option<(Episode, usize)> {
    if b[at] != b's' || !word_start(b, at) {
        return None;
    }
    let (season, end) = digits(b, at + 1)?;
    if end - (at + 1) > 2 {
        return None;
    }
    let mut e = end;
    if e < b.len() && matches!(b[e], b' ' | b'.' | b'-' | b'_') {
        e += 1;
    }
    if e >= b.len() || b[e] != b'e' {
        return None;
    }
    let (episode, end) = digits(b, e + 1)?;
    if end - (e + 1) > 3 {
        return None;
    }
    // "S01E02E03" is a double episode; the first is the one that counts.
    if !word_end(b, end) && b[end] != b'e' {
        return None;
    }
    Some((Episode { season, episode }, end))
}

/// `2x05` at `at`, standing on its own.
fn nxm(b: &[u8], at: usize) -> Option<(Episode, usize)> {
    if !b[at].is_ascii_digit() || !word_start(b, at) {
        return None;
    }
    let (season, end) = digits(b, at)?;
    if end - at > 2 || end >= b.len() || b[end] != b'x' {
        return None;
    }
    let (episode, stop) = digits(b, end + 1)?;
    if stop - (end + 1) > 3 || !word_end(b, stop) {
        return None;
    }
    Some((Episode { season, episode }, stop))
}

/// `season 2 episode 5`, or `season 2, ep 5`, at `at`.
fn words(b: &[u8], at: usize) -> Option<(Episode, usize)> {
    if !b[at..].starts_with(b"season") || !word_start(b, at) {
        return None;
    }
    let (season, end) = digits(b, skip_gap(b, at + 6))?;
    let at = skip_gap(b, end);
    let rest = &b[at..];
    let skip = if rest.starts_with(b"episode") {
        7
    } else if rest.starts_with(b"ep") {
        2
    } else {
        return None;
    };
    let (episode, end) = digits(b, skip_gap(b, at + skip))?;
    Some((Episode { season, episode }, end))
}

/// The first season and episode written in a title, if any.
pub fn find(title: &str) -> Option<Episode> {
    let lower = title.to_ascii_lowercase();
    let b = lower.as_bytes();
    (0..b.len()).find_map(|at| {
        sxxexx(b, at)
            .or_else(|| nxm(b, at))
            .or_else(|| words(b, at))
            .map(|(ep, _)| ep)
    })
}

/// Whether the title says it is a corrected release (REPACK or PROPER), which is taken
/// even when the same episode was downloaded before.
pub fn is_fix(title: &str) -> bool {
    title
        .split(|c: char| !c.is_ascii_alphanumeric())
        .any(|w| w.eq_ignore_ascii_case("repack") || w.eq_ignore_ascii_case("proper"))
}

/// A parsed episode filter. The empty filter lets every title through.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Filter {
    /// From this episode to that one, both included.
    parts: Vec<(Episode, Episode)>,
}

const LAST: u32 = u32::MAX;

/// Reads `S02`, `S02E05` or `2x05`. The episode is `None` for a whole season.
fn point(text: &str) -> Option<(u32, Option<u32>)> {
    let t = text.trim().to_ascii_lowercase();
    let b = t.as_bytes();
    if b.is_empty() {
        return None;
    }
    if b[0] == b's' {
        let (season, end) = digits(b, 1)?;
        if end == b.len() {
            return Some((season, None));
        }
        let mut e = end;
        if matches!(b[e], b' ' | b'.' | b'_') {
            e += 1;
        }
        if e >= b.len() || b[e] != b'e' {
            return None;
        }
        let (episode, end) = digits(b, e + 1)?;
        return (end == b.len()).then_some((season, Some(episode)));
    }
    let (season, end) = digits(b, 0)?;
    if end >= b.len() || b[end] != b'x' {
        return None;
    }
    let (episode, end) = digits(b, end + 1)?;
    (end == b.len()).then_some((season, Some(episode)))
}

fn bad(part: &str) -> String {
    format!("“{part}” is not an episode. Write it like S02, S02E05, S02E05- or S02E05-S02E10.")
}

impl Filter {
    pub fn parse(text: &str) -> Result<Filter, String> {
        let mut parts = Vec::new();
        for raw in text.split([',', ';']) {
            let part = raw.trim();
            if part.is_empty() {
                continue;
            }
            let (from, to) = match part.split_once('-') {
                None => (part, None),
                Some((a, b)) => (a, Some(b.trim())),
            };
            let (season, episode) = point(from).ok_or_else(|| bad(part))?;
            let start = Episode {
                season,
                episode: episode.unwrap_or(0),
            };
            let end = match to {
                // S02 or S02E05
                None => Episode {
                    season,
                    episode: episode.unwrap_or(LAST),
                },
                // S02- or S02E05-
                Some("") => Episode {
                    season: LAST,
                    episode: LAST,
                },
                Some(to) => {
                    if let Some((s, e)) = point(to) {
                        Episode {
                            season: s,
                            episode: e.unwrap_or(LAST),
                        }
                    } else if let Ok(e) = to.parse::<u32>() {
                        // S02E05-10: the same season.
                        if episode.is_none() {
                            return Err(bad(part));
                        }
                        Episode { season, episode: e }
                    } else {
                        return Err(bad(part));
                    }
                }
            };
            if end < start {
                return Err(format!("“{part}” ends before it starts."));
            }
            parts.push((start, end));
        }
        Ok(Filter { parts })
    }

    pub fn is_empty(&self) -> bool {
        self.parts.is_empty()
    }

    /// Whether this episode is one the filter asks for.
    pub fn wants(&self, ep: Episode) -> bool {
        self.parts.iter().any(|(a, b)| *a <= ep && ep <= *b)
    }

    /// Whether a title passes. With a filter set, a title without an episode does not.
    pub fn wants_title(&self, title: &str) -> bool {
        self.is_empty() || find(title).is_some_and(|ep| self.wants(ep))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ep(season: u32, episode: u32) -> Option<Episode> {
        Some(Episode { season, episode })
    }

    #[test]
    fn finds_episodes_in_titles() {
        assert_eq!(find("Show.Name.S02E05.720p.WEB.x264"), ep(2, 5));
        assert_eq!(find("show name s2e5 hdtv"), ep(2, 5));
        assert_eq!(find("Show Name S02 E05"), ep(2, 5));
        assert_eq!(find("Show.Name.S02.E05.1080p"), ep(2, 5));
        assert_eq!(find("Show Name 2x05 [720p]"), ep(2, 5));
        assert_eq!(find("Show Name - 12x103 - Title"), ep(12, 103));
        assert_eq!(find("Show Name Season 2 Episode 5"), ep(2, 5));
        assert_eq!(find("Show Name, season 2, ep. 5"), ep(2, 5));
        assert_eq!(find("Show.S01E02E03.Double"), ep(1, 2));
        assert_eq!(find("[Group] Show - S03E11 (1080p)"), ep(3, 11));
    }

    #[test]
    fn ignores_things_that_only_look_like_episodes() {
        assert_eq!(find("Movie.2019.1920x1080.x264"), None);
        assert_eq!(find("Movie 2019 BluRay x265 10bit"), None);
        assert_eq!(find("Ubuntu 24.04 desktop amd64"), None);
        assert_eq!(find("Show S2024E01"), None);
        assert_eq!(find("Seasons Greetings 2x"), None);
        assert_eq!(find("Classes 101x5"), None);
        assert_eq!(find("SAS2E5"), None);
        assert_eq!(find(""), None);
        assert_eq!(find("s"), None);
        assert_eq!(find("season"), None);
        assert_eq!(find("S02E"), None);
        assert_eq!(find("2x"), None);
    }

    #[test]
    fn tells_fixes_apart() {
        assert!(is_fix("Show.S02E05.REPACK.720p"));
        assert!(is_fix("Show S02E05 proper 1080p"));
        assert!(!is_fix("Show S02E05 Improper Fraction"));
        assert!(!is_fix("Show.S02E05.720p"));
    }

    #[test]
    fn keys_are_the_same_however_written() {
        assert_eq!(find("S2E5").map(|e| e.key()), Some("S02E05".to_owned()));
        assert_eq!(find("2x05").map(|e| e.key()), Some("S02E05".to_owned()));
        assert_eq!(find("Season 2 Episode 5").map(|e| e.key()), Some("S02E05".to_owned()));
    }

    #[test]
    fn whole_season() {
        let f = Filter::parse("S02").unwrap();
        assert!(f.wants_title("Show S02E01"));
        assert!(f.wants_title("Show S02E99"));
        assert!(!f.wants_title("Show S01E10"));
        assert!(!f.wants_title("Show S03E01"));
    }

    #[test]
    fn from_an_episode_on() {
        let f = Filter::parse("S02E05-").unwrap();
        assert!(!f.wants_title("Show S02E04"));
        assert!(f.wants_title("Show S02E05"));
        assert!(f.wants_title("Show S02E06"));
        assert!(f.wants_title("Show S03E01"));
        assert!(!f.wants_title("Show S01E20"));
        let f = Filter::parse("s2-").unwrap();
        assert!(f.wants_title("Show S02E01"));
        assert!(f.wants_title("Show S05E01"));
        assert!(!f.wants_title("Show S01E01"));
    }

    #[test]
    fn ranges() {
        let f = Filter::parse("S02E05-S02E07").unwrap();
        assert!(!f.wants_title("S02E04"));
        assert!(f.wants_title("S02E05"));
        assert!(f.wants_title("S02E07"));
        assert!(!f.wants_title("S02E08"));
        let f = Filter::parse("2x05-10").unwrap();
        assert!(f.wants_title("Show 2x10"));
        assert!(!f.wants_title("Show 2x11"));
        let f = Filter::parse("S01E10-S02E02").unwrap();
        assert!(f.wants_title("S01E11"));
        assert!(f.wants_title("S02E01"));
        assert!(!f.wants_title("S02E03"));
        let f = Filter::parse("S01-S02").unwrap();
        assert!(f.wants_title("S02E40"));
        assert!(!f.wants_title("S03E01"));
    }

    #[test]
    fn several_parts() {
        let f = Filter::parse("S01E03; S02E05-, 3x01").unwrap();
        assert!(f.wants_title("S01E03"));
        assert!(!f.wants_title("S01E04"));
        assert!(f.wants_title("S02E09"));
        assert!(f.wants_title("S03E01"));
    }

    #[test]
    fn single_episode() {
        let f = Filter::parse(" s02e05 ").unwrap();
        assert!(f.wants_title("S02E05"));
        assert!(!f.wants_title("S02E06"));
    }

    #[test]
    fn empty_filter_lets_everything_through() {
        let f = Filter::parse(" , ;").unwrap();
        assert!(f.is_empty());
        assert!(f.wants_title("anything at all"));
    }

    #[test]
    fn title_without_episode_fails_a_filter() {
        let f = Filter::parse("S01-").unwrap();
        assert!(!f.wants_title("Show Special 720p"));
    }

    #[test]
    fn bad_filters_say_why() {
        for text in ["S02E", "hello", "S02E05-S02E", "S02-10", "x05", "S02E05-abc", "2x"] {
            let e = Filter::parse(text).unwrap_err();
            assert!(e.contains("not an episode"), "{text}: {e}");
        }
        let e = Filter::parse("S02E05-S02E01").unwrap_err();
        assert!(e.contains("ends before"), "{e}");
        let e = Filter::parse("S03-S02").unwrap_err();
        assert!(e.contains("ends before"), "{e}");
    }

    #[test]
    fn survives_rubbish() {
        for text in [
            "sssss",
            "s99999e9999999999",
            "9999999999x1",
            "season season episode",
            "é S02E05 ü",
        ] {
            let _ = find(text);
            let _ = Filter::parse(text);
        }
        assert_eq!(find("é S02E05 ü"), ep(2, 5));
    }
}
