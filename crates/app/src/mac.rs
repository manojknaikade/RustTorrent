//! Receiving magnet links and `.torrent` files from macOS.
//!
//! winit owns the application delegate and has no method for opened files, so one is added
//! to its class just before the app finishes launching. Magnet links arrive as a "get URL"
//! Apple Event, which is handled directly.
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, Imp, Sel};
use objc2::{class, define_class, msg_send, sel, AllocAnyThread};
use objc2_foundation::{NSArray, NSNotification, NSObject, NSString, NSURL};
use eframe::egui;
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

/// `applicationShouldTerminate:`. Quit from the menu ends the app without closing the window
/// first, so the window's own question would never be asked. Returns 0 (cancel) or 1 (now).
extern "C-unwind" fn should_terminate(_this: &AnyObject, _sel: Sel, _app: &AnyObject) -> usize {
    if ASK_BEFORE_QUIT.load(Ordering::Relaxed) {
        QUIT_ASKED.store(true, Ordering::Relaxed);
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

#[link(name = "CoreServices", kind = "framework")]
unsafe extern "C" {
    // Both take and give CFString, which is the same object as NSString.
    fn LSCopyDefaultHandlerForURLScheme(scheme: *const NSString) -> *mut NSString;
    fn LSSetDefaultHandlerForURLScheme(scheme: *const NSString, bundle_id: *const NSString) -> i32;
}

/// The bundle identifier of the app that macOS opens magnet links with, if there is one.
pub fn magnet_handler() -> Option<String> {
    let scheme = NSString::from_str("magnet");
    unsafe {
        let id = LSCopyDefaultHandlerForURLScheme(&*scheme);
        Retained::from_raw(id).map(|s| s.to_string())
    }
}

/// Asks macOS to open magnet links with this app from now on. macOS may ask the user to
/// agree. Returns false if macOS refused at once.
pub fn make_magnet_handler() -> bool {
    let scheme = NSString::from_str("magnet");
    let id = NSString::from_str(BUNDLE_ID);
    unsafe { LSSetDefaultHandlerForURLScheme(&*scheme, &*id) == 0 }
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
        if let Event::Key { key, pressed: true, modifiers, .. } = &event {
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
