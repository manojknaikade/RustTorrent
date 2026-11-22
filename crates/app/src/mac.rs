//! Receiving magnet links and `.torrent` files from macOS.
//!
//! winit owns the application delegate and has no method for opened files, so one is added
//! to its class just before the app finishes launching. Magnet links arrive as a "get URL"
//! Apple Event, which is handled directly.
use eframe::egui;
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, Bool, Imp, Sel};
use objc2::{class, define_class, msg_send, sel, AllocAnyThread};
use objc2_foundation::{NSArray, NSNotification, NSObject, NSString, NSURL};
use std::sync::atomic::{AtomicBool, Ordering};

extern "C-unwind" fn open_urls(_this: &AnyObject, _sel: Sel, _app: &AnyObject, urls: &NSArray<NSURL>) {
    for url in urls.iter() {
        if let Some(s) = url.absoluteString() {
            crate::deliver(s.to_string());
        }
    }
}

/// Set by the window while quitting would interrupt a download.
pub static ASK_BEFORE_QUIT: AtomicBool = AtomicBool::new(false);
/// Set when the user chose Quit from the menu (or pressed Cmd-Q) and was not let go yet.
pub static QUIT_ASKED: AtomicBool = AtomicBool::new(false);

/// `applicationShouldHandleReopen:hasVisibleWindows:`: opening the app again (from the
/// Finder, Spotlight or the Dock) brings back a window hidden in the menu bar.
extern "C-unwind" fn should_reopen(_this: &AnyObject, _sel: Sel, _app: &AnyObject, _visible: Bool) -> Bool {
    crate::menu_bar::show_if_hidden();
    Bool::YES
}

/// `applicationShouldTerminate:`. Quit from the menu ends the app without closing the window
/// first, so the window's own question would never be asked. Returns 0 (cancel) or 1 (now).
extern "C-unwind" fn should_terminate(_this: &AnyObject, _sel: Sel, _app: &AnyObject) -> usize {
    if ASK_BEFORE_QUIT.load(Ordering::Relaxed) {
        QUIT_ASKED.store(true, Ordering::Relaxed);
        // The question is asked in the window, which may be hidden in the menu bar.
        crate::menu_bar::show_if_hidden();
        crate::wake();
        0
    } else {
        1
    }
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "RustTorrentLaunchObserver"]
    struct LaunchObserver;

    impl LaunchObserver {
        #[unsafe(method(handleGetURL:withReplyEvent:))]
        fn handle_get_url(&self, event: &AnyObject, _reply: &AnyObject) {
            unsafe {
                // 0x2D2D2D2D is keyDirectObject, the parameter that holds the address.
                let desc: *mut AnyObject = msg_send![event, paramDescriptorForKeyword: 0x2D2D2D2Du32];
                if desc.is_null() {
                    return;
                }
                let s: *mut NSString = msg_send![desc, stringValue];
                if let Some(s) = s.as_ref() {
                    crate::deliver(s.to_string());
                }
            }
        }

        #[unsafe(method(willFinishLaunching:))]
        fn will_finish_launching(&self, _n: &NSNotification) {
            unsafe {
                // 0x4755524C is 'GURL', the "get URL" event class and id.
                let manager: *mut AnyObject = msg_send![class!(NSAppleEventManager), sharedAppleEventManager];
                let _: () = msg_send![manager, setEventHandler: self, andSelector: sel!(handleGetURL:withReplyEvent:), forEventClass: 0x4755524Cu32, andEventID: 0x4755524Cu32];
                crate::scripting::install(manager);

                let app: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
                let delegate: *mut AnyObject = msg_send![app, delegate];
                if delegate.is_null() {
                    eprintln!("rusttorrent: no app delegate at launch; opened files will not arrive");
                    return;
                }
                let cls: *const AnyClass = msg_send![delegate, class];
                let imp: Imp = std::mem::transmute(
                    open_urls as extern "C-unwind" fn(&AnyObject, Sel, &AnyObject, &NSArray<NSURL>),
                );
                let added =
                    objc2::ffi::class_addMethod(cls as *mut AnyClass, sel!(application:openURLs:), imp, c"v@:@@".as_ptr());
                if !added.as_bool() {
                    eprintln!("rusttorrent: could not add application:openURLs:; opened files will not arrive");
                }
                let imp: Imp = std::mem::transmute(should_terminate as extern "C-unwind" fn(&AnyObject, Sel, &AnyObject) -> usize);
                let added = objc2::ffi::class_addMethod(
                    cls as *mut AnyClass,
                    sel!(applicationShouldTerminate:),
                    imp,
                    c"Q@:@".as_ptr(),
                );
                if !added.as_bool() {
                    eprintln!("rusttorrent: could not add applicationShouldTerminate:; quitting will not ask first");
                }
                let imp: Imp =
                    std::mem::transmute(should_reopen as extern "C-unwind" fn(&AnyObject, Sel, &AnyObject, Bool) -> Bool);
                // BOOL is `bool` on Apple silicon and `signed char` on Intel.
                let types = if cfg!(target_arch = "aarch64") { c"B@:@B" } else { c"c@:@c" };
                let added = objc2::ffi::class_addMethod(
                    cls as *mut AnyClass,
                    sel!(applicationShouldHandleReopen:hasVisibleWindows:),
                    imp,
                    types.as_ptr(),
                );
                if !added.as_bool() {
                    eprintln!("rusttorrent: could not add applicationShouldHandleReopen:; reopening will not show the window");
                }
            }
        }
    }
);

pub fn install() {
    let observer: Retained<LaunchObserver> = unsafe { msg_send![LaunchObserver::alloc(), init] };
    let name = NSString::from_str("NSApplicationWillFinishLaunchingNotification");
    unsafe {
        let center: *mut AnyObject = msg_send![class!(NSNotificationCenter), defaultCenter];
        let _: () = msg_send![center, addObserver: &*observer, selector: sel!(willFinishLaunching:), name: &*name, object: std::ptr::null::<AnyObject>()];
    }
    std::mem::forget(observer); // lives for the whole run
    give_back_memory();
    keep_default();
}

/// macOS malloc keeps freed pages and counts them against the app, so after a busy
/// moment (startup, a burst of peers) an idle app could hold 10 MB or more it no longer
/// uses. The pages go back 5 s after start, then once a minute. The engine also hands
/// them back after torrents are removed and after it restarts.
fn give_back_memory() {
    let spawned = std::thread::Builder::new().name("memory".into()).spawn(|| {
        std::thread::sleep(std::time::Duration::from_secs(5));
        loop {
            rusttorrent_core::give_back_memory();
            std::thread::sleep(std::time::Duration::from_secs(60));
        }
    });
    drop(spawned);
}

/// The name macOS knows this app by. It must match the bundle's Info.plist.
pub const BUNDLE_ID: &str = "io.github.manojknaikade.rusttorrent";
/// The type macOS gives `.torrent` files. The bundle declares it too.
const TORRENT_TYPE: &str = "org.bittorrent.torrent";
/// kLSRolesAll: open, edit and view.
const ALL_ROLES: u32 = 0xFFFF_FFFF;

#[link(name = "CoreServices", kind = "framework")]
unsafe extern "C" {
    // These take and give CFString and CFURL, which are the same objects as NSString and NSURL.
    fn LSCopyDefaultHandlerForURLScheme(scheme: *const NSString) -> *mut NSString;
    fn LSSetDefaultHandlerForURLScheme(scheme: *const NSString, bundle_id: *const NSString) -> i32;
    fn LSCopyDefaultRoleHandlerForContentType(kind: *const NSString, role: u32) -> *mut NSString;
    fn LSSetDefaultRoleHandlerForContentType(kind: *const NSString, role: u32, bundle_id: *const NSString) -> i32;
    fn LSRegisterURL(url: *const NSURL, update: u8) -> i32;
}

/// The apps macOS opens magnet links and `.torrent` files with, by bundle identifier.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Handlers {
    pub magnet: Option<String>,
    pub torrent: Option<String>,
}

fn is_us(id: &Option<String>) -> bool {
    id.as_deref().is_some_and(|id| id.eq_ignore_ascii_case(BUNDLE_ID))
}

impl Handlers {
    /// RustTorrent opens both.
    pub fn ours(&self) -> bool {
        is_us(&self.magnet) && is_us(&self.torrent)
    }

    /// Words for who opens them now, for the Settings dialog.
    pub fn describe(&self) -> String {
        let name = |id: &Option<String>| id.as_deref().map(app_name).unwrap_or_else(|| "no app".to_owned());
        match (is_us(&self.magnet), is_us(&self.torrent)) {
            (true, true) => "Magnet links and .torrent files open in RustTorrent.".to_owned(),
            (false, true) => format!("Magnet links open in {}.", name(&self.magnet)),
            (true, false) => format!(".torrent files open in {}.", name(&self.torrent)),
            (false, false) if self.magnet.is_none() && self.torrent.is_none() => {
                "No app is set to open magnet links or .torrent files.".to_owned()
            }
            (false, false) if self.magnet == self.torrent => {
                format!("Magnet links and .torrent files open in {}.", name(&self.magnet))
            }
            (false, false) => format!(
                "Magnet links open in {}, .torrent files in {}.",
                name(&self.magnet),
                name(&self.torrent)
            ),
        }
    }
}

pub fn handlers() -> Handlers {
    let scheme = NSString::from_str("magnet");
    let kind = NSString::from_str(TORRENT_TYPE);
    unsafe {
        let magnet = Retained::from_raw(LSCopyDefaultHandlerForURLScheme(&*scheme)).map(|s| s.to_string());
        let torrent =
            Retained::from_raw(LSCopyDefaultRoleHandlerForContentType(&*kind, ALL_ROLES)).map(|s| s.to_string());
        Handlers { magnet, torrent }
    }
}

/// The name an app shows in the Finder, from its bundle identifier.
fn app_name(id: &str) -> String {
    let ns_id = NSString::from_str(id);
    unsafe {
        let workspace: *mut AnyObject = msg_send![class!(NSWorkspace), sharedWorkspace];
        let url: Option<Retained<NSURL>> = msg_send![workspace, URLForApplicationWithBundleIdentifier: &*ns_id];
        url.and_then(|url| url.URLByDeletingPathExtension())
            .and_then(|url| url.lastPathComponent())
            .map(|name| name.to_string())
            .unwrap_or_else(|| id.to_owned())
    }
}

/// The app bundle this process runs from, if it is RustTorrent's (not `cargo run`).
fn own_bundle() -> Option<Retained<NSURL>> {
    unsafe {
        let bundle: *mut AnyObject = msg_send![class!(NSBundle), mainBundle];
        let id: Option<Retained<NSString>> = msg_send![bundle, bundleIdentifier];
        if !id.is_some_and(|id| id.to_string().eq_ignore_ascii_case(BUNDLE_ID)) {
            return None;
        }
        msg_send![bundle, bundleURL]
    }
}

/// Makes RustTorrent the app for magnet links and `.torrent` files, in place of whatever
/// app (often qBittorrent) has them. The running copy is registered again first, so macOS
/// opens this copy rather than an older one it still remembers.
pub fn make_default() -> Result<(), String> {
    let Some(bundle) = own_bundle() else {
        return Err("Only RustTorrent.app can be the default, not a copy run from a terminal.".to_owned());
    };
    let scheme = NSString::from_str("magnet");
    let kind = NSString::from_str(TORRENT_TYPE);
    let id = NSString::from_str(BUNDLE_ID);
    let (magnet, torrent) = unsafe {
        LSRegisterURL(&*bundle, 1);
        (
            LSSetDefaultHandlerForURLScheme(&*scheme, &*id),
            LSSetDefaultRoleHandlerForContentType(&*kind, ALL_ROLES, &*id),
        )
    };
    let now = handlers();
    if now.ours() {
        return Ok(());
    }
    Err(format!(
        "macOS did not change it (codes {magnet} and {torrent}). {}",
        now.describe()
    ))
}

/// Set from the settings: take magnet links and `.torrent` files back whenever another app
/// makes itself their default. qBittorrent does this each time it starts.
pub static KEEP_DEFAULT: AtomicBool = AtomicBool::new(false);

/// Checks every 20 s, and does nothing while [`KEEP_DEFAULT`] is off.
fn keep_default() {
    if own_bundle().is_none() {
        return;
    }
    let spawned = std::thread::Builder::new().name("default app".into()).spawn(|| loop {
        std::thread::sleep(std::time::Duration::from_secs(20));
        if KEEP_DEFAULT.load(Ordering::Relaxed) && !handlers().ours() {
            if let Err(e) = make_default() {
                eprintln!("rusttorrent: could not take back magnet links: {e}");
            }
        }
    });
    drop(spawned);
}

/// The system pasteboard, text only. egui's own clipboard support also handles images,
/// which brought image decoders into the app.
mod pasteboard {
    use objc2::rc::Retained;
    use objc2::runtime::{AnyObject, Bool};
    use objc2::{class, msg_send};
    use objc2_foundation::NSString;

    const TEXT: &str = "public.utf8-plain-text";

    fn general() -> Option<Retained<AnyObject>> {
        unsafe { msg_send![class!(NSPasteboard), generalPasteboard] }
    }

    pub fn text() -> Option<String> {
        let board = general()?;
        let kind = NSString::from_str(TEXT);
        let text: Option<Retained<NSString>> = unsafe { msg_send![&*board, stringForType: &*kind] };
        text.map(|t| t.to_string())
    }

    pub fn set_text(text: &str) {
        let Some(board) = general() else { return };
        let kind = NSString::from_str(TEXT);
        let text = NSString::from_str(text);
        unsafe {
            let _: isize = msg_send![&*board, clearContents];
            let _: Bool = msg_send![&*board, setString: &*text, forType: &*kind];
        }
    }
}

/// Turns Cmd+C, Cmd+X and Cmd+V (and the dedicated keys) into copy, cut and paste.
/// They arrive as plain keys because the window's own clipboard shortcuts are off.
pub fn clipboard_keys(events: &mut Vec<egui::Event>) {
    use egui::{Event, Key};
    let mut out = Vec::with_capacity(events.len());
    for event in events.drain(..) {
        if let Event::Key {
            key,
            pressed: true,
            modifiers,
            ..
        } = &event
        {
            let cmd = modifiers.command_only();
            match key {
                Key::C if cmd => {
                    out.push(Event::Copy);
                    continue;
                }
                Key::Copy => {
                    out.push(Event::Copy);
                    continue;
                }
                Key::X if cmd => {
                    out.push(Event::Cut);
                    continue;
                }
                Key::Cut => {
                    out.push(Event::Cut);
                    continue;
                }
                Key::V if cmd => {
                    paste(&mut out);
                    continue;
                }
                Key::Paste => {
                    paste(&mut out);
                    continue;
                }
                _ => {}
            }
        }
        out.push(event);
    }
    *events = out;
}

fn paste(out: &mut Vec<egui::Event>) {
    if let Some(text) = pasteboard::text().filter(|t| !t.is_empty()) {
        out.push(egui::Event::Paste(text.replace("\r\n", "\n")));
    }
}

/// Puts text the window copied this frame on the system pasteboard.
pub fn copy_out(ctx: &egui::Context) {
    let copied: Vec<String> = ctx.output(|o| {
        o.commands
            .iter()
            .filter_map(|c| match c {
                egui::OutputCommand::CopyText(text) => Some(text.clone()),
                _ => None,
            })
            .collect()
    });
    if let Some(text) = copied.last() {
        pasteboard::set_text(text);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_and_describes_handlers() {
        let h = handlers();
        let words = h.describe();
        assert!(words.ends_with('.'), "{words}");
        // A test binary is not the app bundle, so it must not change the defaults.
        assert!(own_bundle().is_none());
        assert!(make_default().is_err());
        assert_eq!(handlers(), h);
        println!("{words}");
    }

    #[test]
    fn describes_mixed_handlers() {
        let us = Some(BUNDLE_ID.to_owned());
        let other = Some("org.example.none-such".to_owned());
        let both = Handlers {
            magnet: us.clone(),
            torrent: us.clone(),
        };
        assert!(both.ours());
        assert_eq!(both.describe(), "Magnet links and .torrent files open in RustTorrent.");
        let mixed = Handlers {
            magnet: other.clone(),
            torrent: us,
        };
        assert!(!mixed.ours());
        assert_eq!(mixed.describe(), "Magnet links open in org.example.none-such.");
        let none = Handlers {
            magnet: None,
            torrent: None,
        };
        assert_eq!(none.describe(), "No app is set to open magnet links or .torrent files.");
    }
}
