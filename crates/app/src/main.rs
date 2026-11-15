//! The desktop window. All rules live in `rusttorrent-core`; this crate only draws the
//! latest snapshot and sends commands.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod add_dialog;
mod app;
#[cfg(target_os = "macos")]
mod mac;

use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use eframe::egui;

/// Links and files handed to the app from outside (the system, the command line), waiting
/// for the window to pass them on.
static INBOX: Mutex<Vec<String>> = Mutex::new(Vec::new());
static CTX: OnceLock<egui::Context> = OnceLock::new();

pub(crate) fn deliver(text: String) {
    INBOX.lock().unwrap_or_else(|e| e.into_inner()).push(text);
    if let Some(ctx) = CTX.get() {
        ctx.request_repaint();
    }
}

/// Asks the window to draw again soon.
#[cfg(target_os = "macos")]
pub(crate) fn wake() {
    if let Some(ctx) = CTX.get() {
        ctx.request_repaint();
    }
}

pub(crate) fn take_inbox() -> Vec<String> {
    std::mem::take(&mut *INBOX.lock().unwrap_or_else(|e| e.into_inner()))
}

fn main() -> eframe::Result {
    #[cfg(target_os = "macos")]
    mac::install();

    for arg in std::env::args().skip(1) {
        if !arg.starts_with('-') {
            deliver(arg);
        }
    }

    let data_dir = std::env::var_os("RUSTTORRENT_DATA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(rusttorrent_core::default_data_dir);

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("RustTorrent")
            .with_inner_size([1180.0, 760.0])
            .with_min_inner_size([860.0, 520.0]),
        ..Default::default()
    };
    eframe::run_native(
        "RustTorrent",
        options,
        Box::new(move |cc| {
            let ctx = cc.egui_ctx.clone();
            let _ = CTX.set(ctx.clone());
            let core = rusttorrent_core::Core::start(data_dir, move || ctx.request_repaint());
            Ok(Box::new(app::App::new(core)))
        }),
    )
}
