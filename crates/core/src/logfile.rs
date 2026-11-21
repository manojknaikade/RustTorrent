//! The log on disk: one line per notice. The file stays open between lines, and when it
//! passes `MAX_LEN` it becomes `<name>.1` (replacing an older one) and a new file starts.
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

/// About 10,000 lines.
const MAX_LEN: u64 = 1024 * 1024;

pub(crate) struct LogFile {
    path: PathBuf,
    file: Option<File>,
    /// Bytes in the open file.
    len: u64,
    max_len: u64,
}

impl LogFile {
    pub(crate) fn new(path: PathBuf) -> Self {
        Self::with_limit(path, MAX_LEN)
    }

    fn with_limit(path: PathBuf, max_len: u64) -> Self {
        Self {
            path,
            file: None,
            len: 0,
            max_len,
        }
    }

    /// Adds one line. A file that can't be written is tried again on the next line.
    pub(crate) fn line(&mut self, text: &str) {
        let bytes = text.len() as u64 + 1;
        if self.file.is_some() && self.len > 0 && self.len + bytes > self.max_len {
            self.file = None;
            let _ = std::fs::rename(&self.path, old_path(&self.path));
        }
        if self.file.is_none() {
            let Ok(file) = OpenOptions::new().create(true).append(true).open(&self.path) else {
                return;
            };
            self.len = file.metadata().map_or(0, |m| m.len());
            self.file = Some(file);
            // A file left over from before that is already too long.
            if self.len > 0 && self.len + bytes > self.max_len {
                return self.line(text);
            }
        }
        let Some(file) = self.file.as_mut() else { return };
        if writeln!(file, "{text}").is_ok() {
            self.len += bytes;
        } else {
            self.file = None;
        }
    }
}

fn old_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".1");
    path.with_file_name(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_a_new_file_when_full_and_keeps_one_old() {
        let dir = std::env::temp_dir().join(format!("rt-log-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("rusttorrent.log");
        let mut log = LogFile::with_limit(path.clone(), 20);
        log.line("first line"); // 11 bytes
        log.line("second"); // 18
        log.line("third line"); // would be 29: rotate
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "third line\n");
        assert_eq!(std::fs::read_to_string(dir.join("rusttorrent.log.1")).unwrap(), "first line\nsecond\n");
        log.line("fourth one"); // 22: rotate again, the oldest goes
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "fourth one\n");
        assert_eq!(std::fs::read_to_string(dir.join("rusttorrent.log.1")).unwrap(), "third line\n");

        // A file that is already too long when the app starts.
        drop(log);
        std::fs::write(&path, "x".repeat(30)).unwrap();
        let mut log = LogFile::with_limit(path.clone(), 20);
        log.line("new");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "new\n");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
