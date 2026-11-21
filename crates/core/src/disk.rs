//! Free space on a disk, for the guard that pauses downloads before the disk fills up.

use std::path::Path;

/// Bytes free for an ordinary user on the disk that holds `path`. A folder that does not
/// exist yet is looked up through its nearest parent that does. `None` when the system
/// cannot say, and always on Windows for now.
pub fn free_bytes(path: &Path) -> Option<u64> {
    let mut at = path;
    loop {
        if at.exists() {
            return free_at(at);
        }
        at = at.parent()?;
    }
}

#[cfg(unix)]
fn free_at(path: &Path) -> Option<u64> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let c = CString::new(path.as_os_str().as_bytes()).ok()?;
    // SAFETY: `c` is a valid C string and `out` is plain data the call fills in.
    unsafe {
        let mut out: libc::statvfs = std::mem::zeroed();
        if libc::statvfs(c.as_ptr(), &mut out) != 0 {
            return None;
        }
        // `f_bavail` is what a user who is not root may use; `f_frsize` is its unit.
        #[allow(clippy::unnecessary_cast)]
        Some((out.f_bavail as u64).saturating_mul(out.f_frsize as u64))
    }
}

#[cfg(not(unix))]
fn free_at(_path: &Path) -> Option<u64> {
    None
}

/// The disk is too full to go on downloading: less than `keep_free_mib` MiB is left.
/// 0 turns the guard off.
pub fn too_full(free: u64, keep_free_mib: u64) -> bool {
    keep_free_mib > 0 && free < keep_free_mib.saturating_mul(1024 * 1024)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn threshold() {
        assert!(!too_full(0, 0), "0 is off");
        assert!(too_full(1023 * 1024 * 1024, 1024));
        assert!(!too_full(1024 * 1024 * 1024, 1024));
        assert!(!too_full(u64::MAX, u64::MAX));
    }

    #[cfg(unix)]
    #[test]
    fn reads_free_space() {
        let dir = std::env::temp_dir();
        let free = free_bytes(&dir).expect("free space of the temporary folder");
        assert!(free > 0);
        // A folder that does not exist yet is looked up through its parent.
        let missing = dir.join("rusttorrent-no-such-folder").join("deeper");
        assert!(free_bytes(&missing).is_some());
    }
}
