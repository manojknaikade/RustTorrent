//! The menu-bar icon (macOS). When it is turned on, closing the window hides it instead of
//! quitting, the Dock icon goes away, and downloads go on. The icon's menu shows the
//! speeds and can bring the window back, pause or resume everything, or quit.
//!
//! While the window is hidden only `App::logic` runs, so everything here is driven from it.
//! The menu itself is filled by macOS on the main thread just before it opens, from the
//! latest status kept in [`STATUS`].
use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::Mutex;

use eframe::egui;
use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyObject, Bool, Sel};
use objc2::{class, define_class, msg_send, sel, AllocAnyThread};
use objc2_foundation::{NSObject, NSString};
use rusttorrent_core::{format, Command, Core, Snapshot, State};

/// The window was hidden by closing it while the menu-bar icon is on.
static HIDDEN: AtomicBool = AtomicBool::new(false);
/// Something asked for the window to come back.
static SHOW: AtomicBool = AtomicBool::new(false);
/// A menu choice waiting for `logic` to carry it out.
static ACTION: AtomicU8 = AtomicU8::new(NONE);
const NONE: u8 = 0;
const PAUSE_ALL: u8 = 1;
const RESUME_ALL: u8 = 2;

/// What the menu shows when it opens.
#[derive(Default, PartialEq)]
struct Status {
    lines: Vec<String>,
    can_pause: bool,
    can_resume: bool,
}

static STATUS: Mutex<Option<Status>> = Mutex::new(None);

/// NSApplicationActivationPolicy.
const REGULAR: isize = 0;
const ACCESSORY: isize = 1;

thread_local! {
    /// The status item and the object its menu talks to. Main thread only.
    static ITEM: RefCell<Option<(Retained<AnyObject>, Retained<MenuTarget>)>> = const { RefCell::new(None) };
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "RustTorrentMenuTarget"]
    struct MenuTarget;

    impl MenuTarget {
        /// NSMenuDelegate: called just before the menu opens.
        #[unsafe(method(menuNeedsUpdate:))]
        fn menu_needs_update(&self, menu: &AnyObject) {
            fill(menu, self);
        }

        #[unsafe(method(showWindow:))]
        fn show_window(&self, _sender: &AnyObject) {
            show();
        }

        #[unsafe(method(pauseAll:))]
        fn pause_all(&self, _sender: &AnyObject) {
            ACTION.store(PAUSE_ALL, Ordering::Relaxed);
            crate::wake();
        }

        #[unsafe(method(resumeAll:))]
        fn resume_all(&self, _sender: &AnyObject) {
            ACTION.store(RESUME_ALL, Ordering::Relaxed);
            crate::wake();
        }

        #[unsafe(method(quit:))]
        fn quit(&self, _sender: &AnyObject) {
            // Goes through applicationShouldTerminate:, which asks first while downloading.
            unsafe {
                let app: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
                let _: () = msg_send![app, terminate: std::ptr::null::<AnyObject>()];
            }
        }
    }
);

/// Asks for the window to come back if it was hidden. Safe from any thread.
pub fn show_if_hidden() {
    if HIDDEN.load(Ordering::Relaxed) {
        show();
    }
}

fn show() {
    SHOW.store(true, Ordering::Relaxed);
    crate::wake();
}

/// Whether closing the window should hide it instead of quitting.
pub fn hides_on_close(snap: &Snapshot) -> bool {
    snap.ready && snap.settings.comfort.menu_bar
}

/// Hides the window and the Dock icon. The menu-bar icon stays.
pub fn hide(ctx: &egui::Context) {
    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
    set_policy(ACCESSORY);
    HIDDEN.store(true, Ordering::Relaxed);
}

/// Runs before each frame and while the window is hidden, on the main thread.
pub fn logic(ctx: &egui::Context, snap: &Snapshot, core: &Core) {
    let want = hides_on_close(snap);
    ITEM.with(|item| {
        let mut item = item.borrow_mut();
        match (want, item.is_some()) {
            (true, false) => *item = make_item(),
            (false, true) => {
                if let Some((status_item, _)) = item.take() {
                    unsafe {
                        let bar: *mut AnyObject = msg_send![class!(NSStatusBar), systemStatusBar];
                        let _: () = msg_send![bar, removeStatusItem: &*status_item];
                    }
                }
            }
            _ => {}
        }
    });
    // With the icon gone there would be no way back to a hidden window.
    if !want && snap.ready {
        show_if_hidden();
    }
    if want {
        let status = status(snap);
        let mut kept = STATUS.lock().unwrap_or_else(|e| e.into_inner());
        if kept.as_ref() != Some(&status) {
            *kept = Some(status);
        }
    }

    if SHOW.swap(false, Ordering::Relaxed) {
        HIDDEN.store(false, Ordering::Relaxed);
        set_policy(REGULAR);
        unsafe {
            let app: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
            let _: () = msg_send![app, activateIgnoringOtherApps: Bool::YES];
        }
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
    }

    let keys = |pick: fn(State) -> bool| -> Vec<String> {
        snap.torrents
            .iter()
            .filter(|t| pick(t.state))
            .map(|t| t.key.clone())
            .collect()
    };
    match ACTION.swap(NONE, Ordering::Relaxed) {
        PAUSE_ALL => {
            let keys = keys(pausable);
            if !keys.is_empty() {
                core.send(Command::Pause(keys));
            }
        }
        RESUME_ALL => {
            let keys = keys(|s| s == State::Paused);
            if !keys.is_empty() {
                core.send(Command::Resume(keys));
            }
        }
        _ => {}
    }
}

fn pausable(state: State) -> bool {
    matches!(
        state,
        State::FetchingInfo | State::Checking | State::Downloading | State::Seeding | State::Queued
    )
}

fn status(snap: &Snapshot) -> Status {
    let count = |s: State| snap.torrents.iter().filter(|t| t.state == s).count();
    let mut lines = vec![format!(
        "↓ {}   ↑ {}",
        format::speed(snap.down_bps),
        format::speed(snap.up_bps)
    )];
    let parts: Vec<String> = [
        (count(State::Downloading), "downloading"),
        (count(State::Seeding), "seeding"),
        (count(State::Paused), "paused"),
    ]
    .into_iter()
    .filter(|(n, _)| *n > 0)
    .map(|(n, what)| format!("{n} {what}"))
    .collect();
    lines.push(if snap.torrents.is_empty() {
        "No torrents".to_owned()
    } else if parts.is_empty() {
        format!("{} torrents", snap.torrents.len())
    } else {
        parts.join(", ")
    });
    Status {
        lines,
        can_pause: snap.torrents.iter().any(|t| pausable(t.state)),
        can_resume: snap.torrents.iter().any(|t| t.state == State::Paused),
    }
}

fn set_policy(policy: isize) {
    unsafe {
        let app: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
        let _: Bool = msg_send![app, setActivationPolicy: policy];
    }
}

fn make_item() -> Option<(Retained<AnyObject>, Retained<MenuTarget>)> {
    unsafe {
        let bar: Option<Retained<AnyObject>> = msg_send![class!(NSStatusBar), systemStatusBar];
        let bar = bar?;
        // NSVariableStatusItemLength
        let item: Option<Retained<AnyObject>> = msg_send![&*bar, statusItemWithLength: -1.0f64];
        let item = item?;
        let button: Option<Retained<AnyObject>> = msg_send![&*item, button];
        if let Some(button) = button {
            let symbol = NSString::from_str("arrow.down.circle");
            let label = NSString::from_str("RustTorrent");
            let image: Option<Retained<AnyObject>> =
                msg_send![class!(NSImage), imageWithSystemSymbolName: &*symbol, accessibilityDescription: &*label];
            match image {
                Some(image) => {
                    let _: () = msg_send![&*image, setTemplate: Bool::YES];
                    let _: () = msg_send![&*button, setImage: &*image];
                }
                None => {
                    let _: () = msg_send![&*button, setTitle: &*label];
                }
            }
            let _: () = msg_send![&*button, setToolTip: &*label];
        }
        let target: Retained<MenuTarget> = msg_send![MenuTarget::alloc(), init];
        let title = NSString::from_str("RustTorrent");
        let menu: Allocated<AnyObject> = msg_send![class!(NSMenu), alloc];
        let menu: Retained<AnyObject> = msg_send![menu, initWithTitle: &*title];
        let _: () = msg_send![&*menu, setAutoenablesItems: Bool::NO];
        let _: () = msg_send![&*menu, setDelegate: &*target];
        let _: () = msg_send![&*item, setMenu: &*menu];
        Some((item, target))
    }
}

/// Fills the menu from the latest status. Main thread, called by macOS.
fn fill(menu: &AnyObject, target: &MenuTarget) {
    let status = STATUS.lock().unwrap_or_else(|e| e.into_inner());
    let empty = Status::default();
    let status = status.as_ref().unwrap_or(&empty);
    unsafe {
        let _: () = msg_send![menu, removeAllItems];
        for line in &status.lines {
            add(menu, target, line, None, false);
        }
        if !status.lines.is_empty() {
            separator(menu);
        }
        add(menu, target, "Show RustTorrent", Some(sel!(showWindow:)), true);
        add(menu, target, "Pause All", Some(sel!(pauseAll:)), status.can_pause);
        add(menu, target, "Resume All", Some(sel!(resumeAll:)), status.can_resume);
        separator(menu);
        add(menu, target, "Quit RustTorrent", Some(sel!(quit:)), true);
    }
}

unsafe fn add(menu: &AnyObject, target: &MenuTarget, title: &str, action: Option<Sel>, enabled: bool) {
    let title = NSString::from_str(title);
    let key = NSString::from_str("");
    unsafe {
        let item: Allocated<AnyObject> = msg_send![class!(NSMenuItem), alloc];
        let item: Retained<AnyObject> = msg_send![
            item,
            initWithTitle: &*title,
            action: action,
            keyEquivalent: &*key
        ];
        if action.is_some() {
            let _: () = msg_send![&*item, setTarget: target];
        }
        let _: () = msg_send![&*item, setEnabled: Bool::new(enabled)];
        let _: () = msg_send![menu, addItem: &*item];
    }
}

unsafe fn separator(menu: &AnyObject) {
    unsafe {
        let item: Retained<AnyObject> = msg_send![class!(NSMenuItem), separatorItem];
        let _: () = msg_send![menu, addItem: &*item];
    }
}
