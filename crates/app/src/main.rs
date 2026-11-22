//! The desktop window. All rules live in `rusttorrent-core`; this crate only draws the
//! latest snapshot and sends commands.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod add_dialog;
mod app;
mod comfort;
mod feed_ui;
mod folders;
#[cfg(target_os = "macos")]
mod mac;
#[cfg(target_os = "macos")]
mod mac_extras;
mod make;
#[cfg(target_os = "macos")]
mod menu_bar;
#[cfg(target_os = "macos")]
mod scripting;
mod network;

use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use eframe::egui;

/// Links and files handed to the app from outside (the system, the command line), waiting
/// for the window to pass them on.
static INBOX: Mutex<Vec<String>> = Mutex::new(Vec::new());
static CTX: OnceLock<egui::Context> = OnceLock::new();
/// Where the app keeps its state and log.
pub(crate) static DATA_DIR: OnceLock<PathBuf> = OnceLock::new();

pub(crate) fn deliver(text: String) {
    INBOX.lock().unwrap_or_else(|e| e.into_inner()).push(text);
    #[cfg(target_os = "macos")]
    menu_bar::show_if_hidden();
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
    let _ = DATA_DIR.set(data_dir.clone());
    // The proxy password lives in the Keychain, not in state.json.
    rusttorrent_core::secrets::use_keychain();

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("RustTorrent")
            .with_inner_size([1180.0, 760.0])
            .with_min_inner_size([860.0, 520.0]),
        // One frame in flight instead of two: one window-sized buffer less, and a list
        // of torrents has no GPU work to overlap.
        wgpu_options: eframe::egui_wgpu::WgpuConfiguration {
            surface: eframe::egui_wgpu::SurfaceConfig::LOW_LATENCY,
            ..Default::default()
        },
        // On macOS Cmd+C/X/V reach the app as keys and mac.rs turns them into
        // copy, cut and paste with the system pasteboard.
        clipboard_shortcuts: !cfg!(target_os = "macos"),
        ..Default::default()
    };
    eframe::run_native(
        "RustTorrent",
        options,
        Box::new(move |cc| {
            let ctx = cc.egui_ctx.clone();
            let _ = CTX.set(ctx.clone());
            let core = rusttorrent_core::Core::start(data_dir, move || ctx.request_repaint());
            #[cfg(target_os = "macos")]
            scripting::connect(core.remote());
            Ok(Box::new(app::App::new(core)))
        }),
    )
}
