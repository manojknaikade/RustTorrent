//! Moving a torrent's data to another folder. The engine cannot do this itself, so the
//! worker takes the torrent out of the engine, moves the files with [`move_files`], and
//! adds it back at the new folder.
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// A move that has started and not yet ended. Kept in the torrent's record, so a move
/// that the app quit in the middle of is finished on the next start.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct MoveNote {
    pub from: PathBuf,
    pub to: PathBuf,
    /// The torrent's files, relative to the folder.
    pub files: Vec<PathBuf>,
    /// The files chosen for download, to choose again when it is added back.
    pub only_files: Option<Vec<usize>>,
    /// The download finished just before the move: say so, and run the finish program,
    /// once the data is in its new folder.
    #[serde(default)]
    pub announce: bool,
}

/// Moves each file from `from` to `to`, keeping its place below the folder. Files that
/// are not there (not downloaded, or moved by an earlier try) are skipped, and a file
/// already at the new place is replaced. On a failure the files moved so far are put
/// back, so the data is all in one place either way. Folders left empty are removed.
pub fn move_files(from: &Path, to: &Path, files: &[PathBuf]) -> Result<(), String> {
    if from == to {
        return Ok(());
    }
    let mut moved: Vec<&Path> = Vec::new();
    for rel in files {
        let (src, dst) = (from.join(rel), to.join(rel));
        if !src.is_file() {
            continue;
        }
        if let Err(e) = move_one(&src, &dst) {
            for rel in moved.iter().rev() {
                let _ = move_one(&to.join(rel), &from.join(rel));
            }
            remove_empty_dirs(to, &moved);
            return Err(format!("could not move {}: {e}", src.display()));
        }
        moved.push(rel);
    }
    let all: Vec<&Path> = files.iter().map(PathBuf::as_path).collect();
    remove_empty_dirs(from, &all);
    Ok(())
}

/// Renames, or copies and deletes when the two places are on different disks.
fn move_one(src: &Path, dst: &Path) -> std::io::Result<()> {
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if std::fs::rename(src, dst).is_ok() {
        return Ok(());
    }
    std::fs::copy(src, dst)?;
    std::fs::remove_file(src)
}

/// Removes the folders below `root` that held these files, if they are now empty.
fn remove_empty_dirs(root: &Path, files: &[&Path]) {
    let mut dirs: Vec<PathBuf> = files
        .iter()
        .flat_map(|rel| rel.ancestors().skip(1))
        .filter(|d| !d.as_os_str().is_empty())
        .map(|d| root.join(d))
        .collect();
    // Deepest first, so a parent is tried after its children.
    dirs.sort_by_key(|d| std::cmp::Reverse(d.components().count()));
    dirs.dedup();
    for dir in dirs {
        // Fails when the folder still has something in it, which is what we want.
        let _ = std::fs::remove_dir(dir);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("rusttorrent-relocate-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn moves_files_and_cleans_up() {
        let dir = scratch("moves");
        let (from, to) = (dir.join("a"), dir.join("b"));
        std::fs::create_dir_all(from.join("Show/Extras")).unwrap();
        std::fs::write(from.join("Show/1.mkv"), "one").unwrap();
        std::fs::write(from.join("Show/Extras/x.txt"), "x").unwrap();
        std::fs::write(from.join("other.txt"), "keep").unwrap();
        let files = vec![
            PathBuf::from("Show/1.mkv"),
            PathBuf::from("Show/Extras/x.txt"),
            // Not downloaded: skipped.
            PathBuf::from("Show/2.mkv"),
        ];
        move_files(&from, &to, &files).unwrap();
        assert_eq!(std::fs::read_to_string(to.join("Show/1.mkv")).unwrap(), "one");
        assert_eq!(std::fs::read_to_string(to.join("Show/Extras/x.txt")).unwrap(), "x");
        assert!(!from.join("Show").exists());
        // Files that are not the torrent's stay.
        assert!(from.join("other.txt").exists());
        // Running it again (as after a restart) does nothing harmful.
        move_files(&from, &to, &files).unwrap();
        assert!(to.join("Show/1.mkv").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn puts_files_back_when_one_fails() {
        let dir = scratch("fails");
        let (from, to) = (dir.join("a"), dir.join("b"));
        std::fs::create_dir_all(&from).unwrap();
        std::fs::create_dir_all(&to).unwrap();
        std::fs::write(from.join("1.bin"), "one").unwrap();
        std::fs::write(from.join("2.bin"), "two").unwrap();
        // A folder where the second file should go makes its move fail.
        std::fs::create_dir_all(to.join("2.bin/blocker")).unwrap();
        let files = vec![PathBuf::from("1.bin"), PathBuf::from("2.bin")];
        assert!(move_files(&from, &to, &files).is_err());
        assert_eq!(std::fs::read_to_string(from.join("1.bin")).unwrap(), "one");
        assert_eq!(std::fs::read_to_string(from.join("2.bin")).unwrap(), "two");
        assert!(!to.join("1.bin").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
