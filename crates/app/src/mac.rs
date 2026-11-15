//! Receiving magnet links and `.torrent` files from macOS.
//!
//! winit owns the application delegate and has no method for opened files, so one is added
//! to its class just before the app finishes launching. Magnet links arrive as a "get URL"
//! Apple Event, which is handled directly.
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, Imp, Sel};
use objc2::{class, define_class, msg_send, sel, AllocAnyThread};
use objc2_foundation::{NSArray, NSNotification, NSObject, NSString, NSURL};

extern "C-unwind" fn open_urls(_this: &AnyObject, _sel: Sel, _app: &AnyObject, urls: &NSArray<NSURL>) {
    for url in urls.iter() {
        if let Some(s) = url.absoluteString() {
            crate::deliver(s.to_string());
        }
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
