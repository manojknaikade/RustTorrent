//! The worker's side of moving a torrent's data to another folder. The file work is in
//! [`crate::relocate`].
//!
//! A move goes: write the torrent's file to the data folder, note the move in the
//! torrent's record, take the torrent out of the engine (keeping its files), move the
//! files in the background, then add the torrent back at the new folder from the saved
//! file. The engine checks the data when it is added back. If the app quits midway, the
//! note in the record makes the next start finish the move.
use std::path::PathBuf;
use std::sync::Arc;

use librqbit::api::TorrentIdOrHash;
use librqbit::Session;

use super::{add_to_engine, Worker};
use crate::arbiter::Intent;
use crate::intake::Source;
use crate::manage;
use crate::relocate::{self, MoveNote};

pub(super) type MoveTask = tokio::task::JoinHandle<Result<(), String>>;

impl Worker {
    fn move_file(&self, key: &str) -> PathBuf {
        self.data_dir.join("moving").join(format!("{key}.torrent"))
    }

    fn record_name(&self, key: &str) -> String {
        match self.store.data.torrents.get(key) {
            Some(r) if !r.name.is_empty() => r.name.clone(),
            _ => key.to_owned(),
        }
    }

    pub(super) async fn move_data(&mut self, keys: Vec<String>, dir: PathBuf) {
        let Some(session) = self.session.clone() else {
            return self.notice("Data cannot be moved while all traffic is blocked.", true);
        };
        for key in keys {
            let Some(handle) = self.find(&key) else { continue };
            let name = handle.name().unwrap_or_else(|| key.clone());
            let from = handle.output_folder().to_path_buf();
            if from == dir {
                continue;
            }
            let Ok((torrent, info, files)) = handle.with_metadata(|m| {
                let files: Vec<PathBuf> = (m.file_infos.iter())
                    .filter(|f| !f.attrs.padding)
                    .map(|f| f.relative_filename.clone())
                    .collect();
                (m.torrent_bytes.clone(), m.info_bytes.clone(), files)
            }) else {
                self.notice(format!("{name} cannot be moved before its details arrive."), true);
                continue;
            };
            let bytes = if torrent.is_empty() {
                let mut trackers: Vec<String> = handle.shared().trackers.iter().map(|u| u.to_string()).collect();
                trackers.sort();
                crate::make::from_info(&info, &trackers)
            } else {
                torrent.to_vec()
            };
            let path = self.move_file(&key);
            let written = path
                .parent()
                .map_or(Ok(()), std::fs::create_dir_all)
                .and_then(|()| std::fs::write(&path, bytes));
            if let Err(e) = written {
                self.notice(
                    format!("Could not move {name}: could not write {}: {e}", path.display()),
                    true,
                );
                continue;
            }
            let note = MoveNote {
                from,
                to: dir.clone(),
                files,
                only_files: handle.only_files(),
            };
            let Some(record) = self.store.data.torrents.get_mut(&key) else {
                continue;
            };
            record.moving = Some(note);
            self.save();
            if let Err(e) = session.delete(TorrentIdOrHash::Id(handle.id()), false).await {
                if let Some(record) = self.store.data.torrents.get_mut(&key) {
                    record.moving = None;
                }
                let _ = std::fs::remove_file(&path);
                self.save();
                self.notice(format!("Could not move {name}: {e:#}"), true);
                continue;
            }
            self.notice(format!("Moving {name} to {}", dir.display()), false);
        }
        self.move_tick().await;
    }

    /// Starts the moves that are noted and not running (just asked for, or left over from
    /// the last run), and adds back the torrents whose files are moved. Called every tick.
    pub(super) async fn move_tick(&mut self) {
        let Some(session) = self.session.clone() else { return };
        let notes: Vec<(String, MoveNote)> = (self.store.data.torrents.iter())
            .filter_map(|(k, r)| r.moving.clone().map(|n| (k.clone(), n)))
            .collect();
        for (key, note) in notes {
            match self.moves.get(&key) {
                None => {
                    // The app quit before the torrent was taken out of the engine.
                    if let Some(handle) = self.find(&key) {
                        if let Err(e) = session.delete(TorrentIdOrHash::Id(handle.id()), false).await {
                            let name = self.record_name(&key);
                            self.notice(format!("Could not move {name}: {e:#}"), true);
                            if let Some(record) = self.store.data.torrents.get_mut(&key) {
                                record.moving = None;
                            }
                            self.dirty = true;
                            continue;
                        }
                    }
                    let task =
                        tokio::task::spawn_blocking(move || relocate::move_files(&note.from, &note.to, &note.files));
                    self.moves.insert(key, task);
                }
                Some(task) if task.is_finished() => {
                    let Some(task) = self.moves.remove(&key) else { continue };
                    let result = task.await.unwrap_or_else(|e| Err(e.to_string()));
                    self.finish_move(&session, key, note, result).await;
                }
                Some(_) => {}
            }
        }
    }

    async fn finish_move(&mut self, session: &Arc<Session>, key: String, note: MoveNote, result: Result<(), String>) {
        let name = self.record_name(&key);
        let folder = if result.is_ok() { &note.to } else { &note.from };
        // The arbiter starts it if it should run.
        let paused = self
            .store
            .data
            .torrents
            .get(&key)
            .is_none_or(|r| r.intent != Intent::Run);
        let options = manage::engine_options(folder, paused, note.only_files.clone());
        let path = self.move_file(&key);
        let added = add_to_engine(session, Source::File(path.clone()), options, None).await;
        if let Some(record) = self.store.data.torrents.get_mut(&key) {
            record.moving = None;
        }
        self.save();
        match (added, result) {
            (Ok(_), Ok(())) => {
                let _ = std::fs::remove_file(&path);
                self.notice(format!("Moved {name} to {}", note.to.display()), false);
            }
            (Ok(_), Err(e)) => {
                let _ = std::fs::remove_file(&path);
                self.notice(
                    format!("Could not move {name}: {e}. It stays in {}", note.from.display()),
                    true,
                );
            }
            (Err(e), _) => self.notice(
                format!(
                    "Could not add {name} back after moving it: {e}. Its torrent file is {}",
                    path.display()
                ),
                true,
            ),
        }
    }
}
