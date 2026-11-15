//! The worker's side of making `.torrent` files, saving them, and reading what they say
//! about themselves. The making itself is in [`crate::make`].
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::Arc;

use super::{add_to_engine, AddDone, Worker};
use crate::intake::Source;
use crate::make::{self, Details, MakeOptions, MakeView, Progress};

pub(super) struct Making {
    job: u64,
    name: String,
    opts: MakeOptions,
    progress: Arc<Progress>,
    /// `None` once the work is over.
    task: Option<tokio::task::JoinHandle<Result<(), String>>>,
    result: Option<Result<String, String>>,
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

impl Worker {
    pub(super) fn start_make(&mut self, opts: MakeOptions) {
        if self.making.as_ref().is_some_and(|m| m.task.is_some()) {
            return self.notice("A torrent is already being created.", true);
        }
        self.jobs += 1;
        let progress = Arc::new(Progress::default());
        let task = {
            let (opts, progress) = (opts.clone(), progress.clone());
            tokio::task::spawn_blocking(move || make::make(&opts, &progress).map(|_| ()))
        };
        self.making = Some(Making {
            job: self.jobs,
            name: file_name(&opts.source),
            opts,
            progress,
            task: Some(task),
            result: None,
        });
    }

    pub(super) fn cancel_make(&mut self) {
        if let Some(m) = &self.making {
            m.progress.cancel.store(true, Ordering::Relaxed);
        }
    }

    /// Writes the torrent file the engine keeps for this torrent. For a torrent added from
    /// a link, the engine builds one from the details it fetched.
    pub(super) fn save_torrent_file(&mut self, key: &str, path: PathBuf) {
        let Some(handle) = self.find(key) else {
            return self.notice(
                "The torrent file cannot be saved while the torrent engine is stopped.",
                true,
            );
        };
        let Ok((torrent, info)) = handle.with_metadata(|m| (m.torrent_bytes.clone(), m.info_bytes.clone())) else {
            return self.notice("The details of this torrent have not arrived yet.", true);
        };
        let bytes = if torrent.is_empty() {
            let mut trackers: Vec<String> = handle.shared().trackers.iter().map(|u| u.to_string()).collect();
            trackers.sort();
            make::from_info(&info, &trackers)
        } else {
            torrent.to_vec()
        };
        match std::fs::write(&path, bytes) {
            Ok(()) => self.notice(format!("Saved {}", file_name(&path)), false),
            Err(e) => self.notice(format!("Could not save {}: {e}", path.display()), true),
        }
    }

    /// Notices when the work is over, starts seeding if asked, and returns what the
    /// snapshot shows. Called on every tick.
    pub(super) async fn make_tick(&mut self) -> (Option<MakeView>, Arc<HashMap<String, Details>>) {
        if let Some(m) = self.making.as_mut() {
            if m.task.as_ref().is_some_and(|t| t.is_finished()) {
                let result = match m.task.take() {
                    Some(task) => task.await.unwrap_or_else(|e| Err(e.to_string())),
                    None => Err("stopped".into()),
                };
                let saved = m.opts.save_to.clone();
                let (seed, source) = (m.opts.seed, m.opts.source.clone());
                m.result = Some(result.clone().map(|()| saved.display().to_string()));
                let name = m.name.clone();
                match result {
                    Ok(()) => {
                        self.notice(format!("Created {}", file_name(&saved)), false);
                        if seed {
                            self.seed_made(saved, &source, name);
                        }
                    }
                    Err(e) if e == "cancelled" => self.notice(format!("Stopped creating a torrent of {name}."), false),
                    Err(e) => self.notice(format!("Could not create a torrent of {name}: {e}"), true),
                }
            }
        }
        let view = self.making.as_ref().map(|m| MakeView {
            job: m.job,
            name: m.name.clone(),
            done: m.progress.done.load(Ordering::Relaxed),
            total: m.progress.total.load(Ordering::Relaxed),
            result: m.result.clone(),
        });
        self.refresh_details();
        (view, self.details.clone())
    }

    /// Adds the torrent just made, pointing at the files it was made from, so that it
    /// checks them and seeds.
    fn seed_made(&mut self, torrent: PathBuf, source: &Path, name: String) {
        let Some(session) = self.session.clone() else {
            return self.notice(
                format!("Could not start seeding {name}: the torrent engine is stopped. Add the torrent file later to seed it."),
                true,
            );
        };
        // The engine puts the files straight into this folder, without a folder of the
        // torrent's name, so a folder is its own place and a single file sits beside it.
        let folder = if source.is_dir() {
            source.to_path_buf()
        } else {
            source.parent().map(Path::to_path_buf).unwrap_or_default()
        };
        let id = self.next_pending;
        self.next_pending += 1;
        let tx = self.add_tx.clone();
        let text = torrent.display().to_string();
        tokio::spawn(async move {
            let result = add_to_engine(&session, Source::File(torrent), folder.to_string_lossy().into_owned()).await;
            let _ = tx.send(AddDone {
                id,
                text,
                label: name,
                result,
            });
        });
    }

    /// Reads each torrent's file once, the first time its details are there.
    fn refresh_details(&mut self) {
        if self.session.is_none() {
            return;
        }
        let handles = self.handles();
        let keys: Vec<String> = handles.iter().map(|h| h.info_hash().as_string()).collect();
        let stale = self.details.keys().any(|k| !keys.contains(k));
        let missing: Vec<_> = handles
            .iter()
            .zip(&keys)
            .filter(|(_, k)| !self.details.contains_key(*k))
            .filter_map(|(h, k)| {
                h.with_metadata(|m| {
                    make::details(&m.torrent_bytes).unwrap_or_else(|| Details {
                        piece_len: m.info.info().piece_length,
                        pieces: m.info.lengths().total_pieces(),
                        ..Default::default()
                    })
                })
                .ok()
                .map(|d| (k.clone(), d))
            })
            .collect();
        if stale || !missing.is_empty() {
            let details = Arc::make_mut(&mut self.details);
            details.retain(|k, _| keys.contains(k));
            details.extend(missing);
        }
    }
}
