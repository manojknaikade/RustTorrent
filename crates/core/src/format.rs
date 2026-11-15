//! Text for sizes, speeds and times.

/// Sizes in decimal units, the way Finder shows them.
pub fn bytes(b: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let (mut v, mut i) = (b as f64, 0);
    while v >= 999.95 && i < 4 {
        v /= 1000.0;
        i += 1;
    }
    if i == 0 {
        format!("{b} B")
    } else {
        format!("{v:.1} {}", UNITS[i])
    }
}

pub fn speed(bytes_per_sec: u64) -> String {
    if bytes_per_sec == 0 {
        "–".into()
    } else {
        format!("{}/s", bytes(bytes_per_sec))
    }
}

pub fn duration(secs: u64) -> String {
    match secs {
        s if s >= 86_400 => format!("{}d {}h", s / 86_400, s % 86_400 / 3600),
        s if s >= 3600 => format!("{}h {}m", s / 3600, s % 3600 / 60),
        s if s >= 60 => format!("{}m {}s", s / 60, s % 60),
        s => format!("{s}s"),
    }
}

/// Time left, or a dash when it cannot be estimated.
pub fn eta(remaining: u64, bytes_per_sec: u64) -> String {
    if remaining == 0 || bytes_per_sec == 0 {
        "–".into()
    } else {
        duration(remaining / bytes_per_sec)
    }
}

pub fn ratio(uploaded: u64, downloaded: u64) -> String {
    if downloaded == 0 {
        "–".into()
    } else {
        format!("{:.2}", uploaded as f64 / downloaded as f64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes() {
        assert_eq!(bytes(0), "0 B");
        assert_eq!(bytes(999), "999 B");
        assert_eq!(bytes(1000), "1.0 KB");
        assert_eq!(bytes(999_960), "1.0 MB");
        assert_eq!(bytes(1_500_000_000), "1.5 GB");
        assert_eq!(bytes(u64::MAX), "18446744.1 TB");
    }

    #[test]
    fn speeds_and_times() {
        assert_eq!(speed(0), "–");
        assert_eq!(speed(2_500_000), "2.5 MB/s");
        assert_eq!(duration(59), "59s");
        assert_eq!(duration(61), "1m 1s");
        assert_eq!(duration(3_700), "1h 1m");
        assert_eq!(duration(90_000), "1d 1h");
        assert_eq!(eta(1000, 0), "–");
        assert_eq!(eta(0, 10), "–");
        assert_eq!(eta(6000, 100), "1m 0s");
    }

    #[test]
    fn ratios() {
        assert_eq!(ratio(5, 0), "–");
        assert_eq!(ratio(150, 100), "1.50");
    }
}
