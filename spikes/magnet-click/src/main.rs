//! Test: can an egui (eframe/winit) app receive magnet links and .torrent files from macOS?
//!
//! macOS delivers both by calling `application:openURLs:` on the app delegate. winit 0.30 owns
//! the delegate and does not implement that method, so this adds it to winit's delegate class
//! just before the app finishes launching, which is before macOS hands over the first link.
use std::sync::{Mutex, OnceLock};

use eframe::egui;

static RECEIVED: Mutex<Vec<String>> = Mutex::new(Vec::new());
static CTX: OnceLock<egui::Context> = OnceLock::new();

fn record(item: String) {
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(std::env::temp_dir().join("rusttorrent-magnet-test.log"))
    {
        let _ = writeln!(f, "{item}");
    }
    RECEIVED.lock().unwrap().push(item);
    if let Some(ctx) = CTX.get() {
        ctx.request_repaint();
    }
}

#[cfg(target_os = "macos")]
mod mac {
    use objc2::rc::Retained;
    use objc2::runtime::{AnyClass, AnyObject, Imp, Sel};
    use objc2::{class, define_class, msg_send, sel, AllocAnyThread};
    use objc2_foundation::{NSArray, NSNotification, NSObject, NSString, NSURL};

    extern "C-unwind" fn open_urls(_this: &AnyObject, _sel: Sel, _app: &AnyObject, urls: &NSArray<NSURL>) {
        for url in urls.iter() {
            if let Some(s) = url.absoluteString() {
                super::record(s.to_string());
            }
        }
    }

    define_class!(
        #[unsafe(super(NSObject))]
        #[name = "RustTorrentLaunchObserver"]
        struct LaunchObserver;

        impl LaunchObserver {
            /// Magnet links arrive as a "get URL" Apple Event. AppKit only forwards it to
            /// `application:openURLs:` if the delegate had that method from the start, so
            /// this handles the event directly.
            #[unsafe(method(handleGetURL:withReplyEvent:))]
            fn handle_get_url(&self, event: &AnyObject, _reply: &AnyObject) {
                unsafe {
                    let desc: *mut AnyObject = msg_send![event, paramDescriptorForKeyword: 0x2D2D2D2Du32]; // keyDirectObject
                    if desc.is_null() { return; }
                    let s: *mut NSString = msg_send![desc, stringValue];
                    if let Some(s) = s.as_ref() {
                        super::record(s.to_string());
                    }
                }
            }

            #[unsafe(method(willFinishLaunching:))]
            fn will_finish_launching(&self, _n: &NSNotification) {
                unsafe {
                    let mgr: *mut AnyObject = msg_send![class!(NSAppleEventManager), sharedAppleEventManager];
                    let _: () = msg_send![mgr, setEventHandler: self, andSelector: sel!(handleGetURL:withReplyEvent:), forEventClass: 0x4755524Cu32, andEventID: 0x4755524Cu32]; // 'GURL'
                    let app: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
                    let delegate: *mut AnyObject = msg_send![app, delegate];
                    if delegate.is_null() {
                        super::record("ERROR: no app delegate at launch".into());
                        return;
                    }
                    let cls: *const AnyClass = msg_send![delegate, class];
                    let imp: Imp = std::mem::transmute(
                        open_urls as extern "C-unwind" fn(&AnyObject, Sel, &AnyObject, &NSArray<NSURL>),
                    );
                    let added = objc2::ffi::class_addMethod(
                        cls as *mut AnyClass,
                        sel!(application:openURLs:),
                        imp,
                        c"v@:@@".as_ptr(),
                    );
                    if !added.as_bool() {
                        super::record("ERROR: could not add application:openURLs:".into());
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
    }
}

struct App;

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ui, |ui| {
            ui.heading("Magnet-click test");
            ui.label("Links and files received from macOS:");
            ui.separator();
            let items = RECEIVED.lock().unwrap();
            if items.is_empty() {
                ui.label("(nothing yet)");
            }
            for (i, item) in items.iter().enumerate() {
                ui.label(format!("{}. {item}", i + 1));
            }
        });
    }
}

fn main() -> eframe::Result {
    #[cfg(target_os = "macos")]
    mac::install();
    eframe::run_native(
        "Magnet Test",
        eframe::NativeOptions::default(),
        Box::new(|cc| {
            let _ = CTX.set(cc.egui_ctx.clone());
            Ok(Box::new(App))
        }),
    )
}
