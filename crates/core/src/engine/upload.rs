//! `.torrent` files sent from the remote-control page.
//!
//! The file is kept under the app's own data folder, named by its info-hash, only until
//! the engine has taken it. The browser never chooses where anything is written.
use std::path::{Path, PathBuf};

use super::Worker;
use crate::manage::AddOptions;

const FOLDER: &str = "uploads";

impl Worker {
    fn uploads_dir(&self) -> PathBuf {
        self.data_dir.join(FOLDER)
    }

    pub(super) fn add_upload(&mut self, bytes: Vec<u8>, category: Option<String>, paused: bool) {
        // The remote page checked it already; this is the only place it is written, so
        // check again rather than trust the caller.
        let hash = match librqbit::torrent_from_bytes(&bytes) {
            Ok(t) => t.info_hash.as_string(),
            Err(_) => return self.notice("The uploaded file is not a torrent.", true),
        };
        let dir = self.uploads_dir();
        let path = dir.join(format!("{hash}.torrent"));
        if let Err(e) = std::fs::create_dir_all(&dir).and_then(|()| std::fs::write(&path, &bytes)) {
            return self.notice(format!("Could not keep the uploaded torrent: {e}"), true);
        }
        self.add_with(AddOptions {
            text: path.to_string_lossy().into_owned(),
            category,
            paused,
            ..Default::default()
        });
    }

    /// Deletes an uploaded file once it has been added, or given up on.
    pub(super) fn forget_upload(&self, text: &str) {
        let path = Path::new(text);
        if path.parent() == Some(self.uploads_dir().as_path()) {
            let _ = std::fs::remove_file(path);
        }
    }
}
