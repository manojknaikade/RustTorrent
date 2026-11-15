//! IP block lists in the plain-text forms people download, turned into one list of
//! address ranges.
//!
//! Read line by line. Each line may be:
//! - `name:1.2.3.4-1.2.3.255` (PeerGuardian / P2P form; the name may contain `:`),
//! - `1.2.3.4 - 1.2.3.255 , 000 , name` (eMule / DAT form; a level of 128 or more means
//!   the range is allowed, so it is skipped),
//! - `1.2.3.0/24` or `2001:db8::/32` (CIDR), with or without a `name:` in front,
//! - a single address, or a bare `start-end` range.
//!
//! Blank lines and lines starting with `#` or `//` are ignored. IPv4 parts with leading
//! zeros (`001.002.003.004`, common in DAT files) are read as plain numbers. The torrent
//! engine reads the result written by [`render`] (its own `name:start-end` form), so every
//! format above works with it.
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// Ranges, both ends included, sorted and with overlaps merged.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct List {
    pub v4: Vec<(u32, u32)>,
    pub v6: Vec<(u128, u128)>,
    /// Lines that are neither empty, a comment, nor a range that could be read.
    pub bad_lines: usize,
}

impl List {
    pub fn len(&self) -> usize {
        self.v4.len() + self.v6.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Adds one address, as from a banned peer.
    pub fn add(&mut self, ip: IpAddr) {
        match ip {
            IpAddr::V4(a) => self.v4.push((a.to_bits(), a.to_bits())),
            IpAddr::V6(a) => self.v6.push((a.to_bits(), a.to_bits())),
        }
    }

    /// Adds every range of another list.
    pub fn extend(&mut self, other: List) {
        self.v4.extend(other.v4);
        self.v6.extend(other.v6);
        self.bad_lines += other.bad_lines;
    }

    /// Sorts the ranges and merges those that overlap or touch.
    pub fn tidy(&mut self) {
        merge(&mut self.v4, |n| n.saturating_add(1));
        merge(&mut self.v6, |n| n.saturating_add(1));
    }

    pub fn contains(&self, ip: IpAddr) -> bool {
        match ip.to_canonical() {
            IpAddr::V4(a) => self.v4.iter().any(|&(s, e)| (s..=e).contains(&a.to_bits())),
            IpAddr::V6(a) => self.v6.iter().any(|&(s, e)| (s..=e).contains(&a.to_bits())),
        }
    }
}

/// `next` gives the number after one, so ranges that touch become one too.
fn merge<T: Ord + Copy>(ranges: &mut Vec<(T, T)>, next: fn(T) -> T) {
    ranges.sort_unstable();
    let mut out: Vec<(T, T)> = Vec::with_capacity(ranges.len());
    for &(s, e) in ranges.iter() {
        match out.last_mut() {
            Some(last) if s <= next(last.1) => last.1 = last.1.max(e),
            _ => out.push((s, e)),
        }
    }
    *ranges = out;
}

/// Reads every line of a block list. Lines that cannot be read are counted, not fatal.
pub fn parse(text: &str) -> List {
    let mut list = List::default();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with("//") {
            continue;
        }
        match parse_line(line) {
            Some(Range::V4(s, e)) => list.v4.push((s, e)),
            Some(Range::V6(s, e)) => list.v6.push((s, e)),
            Some(Range::Allowed) => {}
            None => list.bad_lines += 1,
        }
    }
    list.tidy();
    list
}

/// The list in the torrent engine's own form, one `name:start-end` line per range.
pub fn render(list: &List) -> String {
    let mut out = String::with_capacity(list.len() * 32);
    for &(s, e) in &list.v4 {
        out.push_str(&format!("r:{}-{}\n", Ipv4Addr::from_bits(s), Ipv4Addr::from_bits(e)));
    }
    for &(s, e) in &list.v6 {
        out.push_str(&format!("r:{}-{}\n", Ipv6Addr::from_bits(s), Ipv6Addr::from_bits(e)));
    }
    out
}

#[derive(Debug, PartialEq, Eq)]
enum Range {
    V4(u32, u32),
    V6(u128, u128),
    /// A DAT line whose level says the range is allowed.
    Allowed,
}

fn parse_line(line: &str) -> Option<Range> {
    // eMule / DAT form: "start - end , level , name".
    if let Some((ips, rest)) = line.split_once(',') {
        if let Some(r) = range(ips.trim()) {
            let level = rest
                .split(',')
                .next()
                .map(str::trim)
                .and_then(|l| l.parse::<u32>().ok());
            return Some(if level.is_some_and(|l| l >= 128) {
                Range::Allowed
            } else {
                r
            });
        }
    }
    if let Some(r) = range(line) {
        return Some(r);
    }
    // A name in front, separated by ':'. The name may contain ':' itself, so try each
    // place from the left until the rest reads as a range.
    let mut from = 0;
    while let Some(i) = line[from..].find(':') {
        let at = from + i;
        // `1.2.3.4-2001:db8::1` must not read as a name `1.2.3.4-2001` and `db8::1`:
        // a ':' right after `-` and hex digits is inside an IPv6 address.
        let inside_v6 = line[..at]
            .rsplit_once('-')
            .is_some_and(|(_, tail)| !tail.trim().is_empty() && tail.trim().chars().all(|c| c.is_ascii_hexdigit()));
        match range(line[at + 1..].trim()) {
            Some(Range::V6(..)) if inside_v6 => {}
            Some(r) => return Some(r),
            None => {}
        }
        from = at + 1;
    }
    None
}

/// `start-end`, `address/prefix` or one address.
fn range(text: &str) -> Option<Range> {
    if let Some((start, end)) = text.split_once('-') {
        return match (ip(start.trim())?, ip(end.trim())?) {
            (IpAddr::V4(s), IpAddr::V4(e)) if s <= e => Some(Range::V4(s.to_bits(), e.to_bits())),
            (IpAddr::V6(s), IpAddr::V6(e)) if s <= e => Some(Range::V6(s.to_bits(), e.to_bits())),
            _ => None,
        };
    }
    if let Some((addr, bits)) = text.split_once('/') {
        let bits: u32 = bits.trim().parse().ok()?;
        return match ip(addr.trim())? {
            IpAddr::V4(a) if bits <= 32 => {
                let mask = if bits == 0 { 0 } else { u32::MAX << (32 - bits) };
                let s = a.to_bits() & mask;
                Some(Range::V4(s, s | !mask))
            }
            IpAddr::V6(a) if bits <= 128 => {
                let mask = if bits == 0 { 0 } else { u128::MAX << (128 - bits) };
                let s = a.to_bits() & mask;
                Some(Range::V6(s, s | !mask))
            }
            _ => None,
        };
    }
    match ip(text)? {
        IpAddr::V4(a) => Some(Range::V4(a.to_bits(), a.to_bits())),
        IpAddr::V6(a) => Some(Range::V6(a.to_bits(), a.to_bits())),
    }
}

/// An address. IPv4 parts may have leading zeros. An IPv4 address written as IPv6
/// (`::ffff:1.2.3.4`) counts as IPv4, which is how the engine sees peers.
pub fn ip(text: &str) -> Option<IpAddr> {
    let text = text.trim().trim_start_matches('[').trim_end_matches(']');
    if text.contains(':') {
        return text.parse::<Ipv6Addr>().ok().map(|a| IpAddr::V6(a).to_canonical());
    }
    let mut parts = [0u8; 4];
    let mut n = 0;
    for part in text.split('.') {
        if n == 4 || part.is_empty() || part.len() > 3 || !part.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        parts[n] = part.parse().ok()?;
        n += 1;
    }
    (n == 4).then(|| IpAddr::V4(Ipv4Addr::from(parts)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v4(a: &str) -> u32 {
        a.parse::<Ipv4Addr>().unwrap().to_bits()
    }

    fn has(list: &List, a: &str) -> bool {
        list.contains(a.parse().unwrap())
    }

    #[test]
    fn p2p_form() {
        let list = parse("Some ISP:1.2.3.4-1.2.3.255\n");
        assert_eq!(list.v4, [(v4("1.2.3.4"), v4("1.2.3.255"))]);
        assert_eq!(list.bad_lines, 0);
    }

    #[test]
    fn name_with_colons_and_spaces() {
        let list = parse("Bad: people: here : 10.0.0.1 - 10.0.0.9\n");
        assert_eq!(list.v4, [(v4("10.0.0.1"), v4("10.0.0.9"))]);
    }

    #[test]
    fn dat_form_with_levels_and_leading_zeros() {
        let text = "001.002.003.000 - 001.002.003.255 , 000 , blocked\n\
                    009.009.009.000 - 009.009.009.255 , 200 , allowed\n";
        let list = parse(text);
        assert_eq!(list.v4, [(v4("1.2.3.0"), v4("1.2.3.255"))]);
        assert_eq!(list.bad_lines, 0);
    }

    #[test]
    fn cidr_v4_and_v6() {
        let list = parse("192.168.0.0/16\nnet:10.1.2.3/24\n2001:db8::/32\n0.0.0.0/0\n");
        assert!(has(&list, "192.168.200.1"));
        assert!(has(&list, "10.1.2.200"));
        assert!(has(&list, "2001:db8:ffff::1"));
        assert!(!has(&list, "2001:db9::1"));
        // 0.0.0.0/0 swallows every other IPv4 range.
        assert_eq!(list.v4, [(0, u32::MAX)]);
    }

    #[test]
    fn ipv6_ranges_with_and_without_name() {
        let list = parse("localv6:2001:db8::1-2001:db8::ffff\n2001:db8:1::1 - 2001:db8:1::2\n");
        assert!(has(&list, "2001:db8::abc"));
        assert!(has(&list, "2001:db8:1::2"));
        assert!(!has(&list, "2001:db8:1::3"));
        assert_eq!(list.bad_lines, 0);
    }

    #[test]
    fn single_addresses_comments_and_bad_lines() {
        let text = "# comment\n// also a comment\n\n8.8.8.8\n[::1]\nnot an address\n1.2.3.4-1.2.3.1\n\
                    1.2.3.4-2001:db8::1\n300.1.1.1\n1.2.3/24\n";
        let list = parse(text);
        assert!(has(&list, "8.8.8.8"));
        assert!(has(&list, "::1"));
        assert!(!has(&list, "8.8.8.9"));
        assert_eq!(list.len(), 2);
        // Backwards range, mixed families, a part over 255, a short address.
        assert_eq!(list.bad_lines, 5);
    }

    #[test]
    fn overlapping_and_touching_ranges_merge() {
        let list = parse("1.0.0.0-1.0.0.10\n1.0.0.5-1.0.0.20\n1.0.0.21-1.0.0.30\n1.0.0.40-1.0.0.50\n");
        assert_eq!(
            list.v4,
            [(v4("1.0.0.0"), v4("1.0.0.30")), (v4("1.0.0.40"), v4("1.0.0.50"))]
        );
    }

    #[test]
    fn mapped_ipv4_counts_as_ipv4() {
        assert_eq!(ip("::ffff:1.2.3.4"), Some("1.2.3.4".parse().unwrap()));
        let mut list = List::default();
        list.add(ip("::ffff:5.6.7.8").unwrap());
        assert!(has(&list, "5.6.7.8"));
    }

    #[test]
    fn render_round_trips() {
        let mut list = parse("a:1.2.3.4-1.2.3.255\n2001:db8::/48\n");
        list.add("9.9.9.9".parse().unwrap());
        list.tidy();
        let text = render(&list);
        assert_eq!(parse(&text), list);
        // Every line is the engine's `name:start-end` form.
        assert!(text.lines().all(|l| l.starts_with("r:") && l.contains('-')));
    }
}
