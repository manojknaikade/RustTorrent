//! Mac notifications and keeping the Mac awake.
//!
//! Notifications go through the UserNotifications framework. It only works when the app runs
//! from its bundle (RustTorrent.app), and macOS asks the user once whether to allow them.
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Mutex;

use block2::{Block, RcBlock};
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, Bool};
use objc2::{class, define_class, msg_send, AllocAnyThread};
use objc2_foundation::{NSObject, NSString};

const UNASKED: u8 = 0;
const ASKING: u8 = 1;
const ALLOWED: u8 = 2;
const REFUSED: u8 = 3;
const UNAVAILABLE: u8 = 4;

static NOTES: AtomicU8 = AtomicU8::new(UNASKED);
/// What macOS said went wrong when asked about notifications, if anything.
static NOTES_ERROR: Mutex<Option<String>> = Mutex::new(None);

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "RustTorrentNotificationDelegate"]
    struct NoteDelegate;

    impl NoteDelegate {
        /// Without this, macOS hides notifications while the app is in front.
        #[unsafe(method(userNotificationCenter:willPresentNotification:withCompletionHandler:))]
        fn will_present(&self, _center: &AnyObject, _note: &AnyObject, done: &Block<dyn Fn(usize)>) {
            // Banner (16), list (8) and sound (2).
            done.call((16 | 8 | 2,));
        }
    }
);

fn class_named(name: &std::ffi::CStr) -> Option<&'static AnyClass> {
    if let Some(cls) = AnyClass::get(name) {
        return Some(cls);
    }
    let path = NSString::from_str("/System/Library/Frameworks/UserNotifications.framework");
    unsafe {
        let bundle: *mut AnyObject = msg_send![class!(NSBundle), bundleWithPath: &*path];
        if !bundle.is_null() {
            let _: Bool = msg_send![bundle, load];
        }
    }
    AnyClass::get(name)
}

/// The notification center, or `None` when the app does not run from its bundle (macOS
/// stops a program that asks for one without a bundle identifier).
fn center() -> Option<&'static AnyObject> {
    unsafe {
        let bundle: *mut AnyObject = msg_send![class!(NSBundle), mainBundle];
        if bundle.is_null() {
            return None;
        }
        let id: *mut NSString = msg_send![bundle, bundleIdentifier];
        if id.is_null() {
            return None;
        }
        let cls = class_named(c"UNUserNotificationCenter")?;
        let center: *mut AnyObject = msg_send![cls, currentNotificationCenter];
        center.as_ref()
    }
}

/// Asks macOS once whether notifications may be shown. The first time, macOS asks the user.
pub fn ask_for_notifications() {
    if NOTES
        .compare_exchange(UNASKED, ASKING, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return;
    }
    let Some(center) = center() else {
        NOTES.store(UNAVAILABLE, Ordering::SeqCst);
        return;
    };
    let delegate: Retained<NoteDelegate> = unsafe { msg_send![NoteDelegate::alloc(), init] };
    let answer = RcBlock::new(|granted: Bool, error: *mut AnyObject| {
        NOTES.store(if granted.as_bool() { ALLOWED } else { REFUSED }, Ordering::SeqCst);
        if let Some(error) = unsafe { error.as_ref() } {
            let text: *mut NSString = unsafe { msg_send![error, localizedDescription] };
            if let Some(text) = unsafe { text.as_ref() } {
                *NOTES_ERROR.lock().unwrap_or_else(|e| e.into_inner()) = Some(text.to_string());
            }
        }
    });
    unsafe {
        let _: () = msg_send![center, setDelegate: &*delegate];
        // Alert (4) and sound (2).
        let _: () = msg_send![center, requestAuthorizationWithOptions: 4usize | 2, completionHandler: &*answer];
    }
    std::mem::forget(delegate); // the center does not keep it alive
}

/// Shows a notification. Does nothing unless macOS has allowed them.
pub fn notify(title: &str, body: &str) {
    if NOTES.load(Ordering::SeqCst) != ALLOWED {
        return;
    }
    let (Some(center), Some(content_cls), Some(request_cls), Some(sound_cls)) = (
        center(),
        class_named(c"UNMutableNotificationContent"),
        class_named(c"UNNotificationRequest"),
        class_named(c"UNNotificationSound"),
    ) else {
        return;
    };
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let id = NSString::from_str(&format!("finished-{}", NEXT.fetch_add(1, Ordering::Relaxed)));
    let (title, body) = (NSString::from_str(title), NSString::from_str(body));
    unsafe {
        let content: Retained<AnyObject> = msg_send![content_cls, new];
        let _: () = msg_send![&*content, setTitle: &*title];
        let _: () = msg_send![&*content, setBody: &*body];
        let sound: *mut AnyObject = msg_send![sound_cls, defaultSound];
        let _: () = msg_send![&*content, setSound: sound];
        let request: *mut AnyObject = msg_send![
            request_cls,
            requestWithIdentifier: &*id,
            content: &*content,
            trigger: std::ptr::null::<AnyObject>()
        ];
        if request.is_null() {
            return;
        }
        let _: () = msg_send![center, addNotificationRequest: request, withCompletionHandler: std::ptr::null::<Block<dyn Fn(*mut AnyObject)>>()];
    }
}

/// Why notifications cannot be shown, in words for the user, or `None` if they can (or
/// macOS has not answered yet).
pub fn notification_problem() -> Option<String> {
    let why = match NOTES.load(Ordering::SeqCst) {
        REFUSED => "macOS does not allow RustTorrent to show notifications. \
                    You can allow them in System Settings, under Notifications."
            .to_owned(),
        UNAVAILABLE => "Notifications only work when RustTorrent is opened as an app.".to_owned(),
        _ => return None,
    };
    match NOTES_ERROR.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
        Some(error) => Some(format!("{why} ({error})")),
        None => Some(why),
    }
}

#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    // Both take CFString, which is the same object as NSString.
    fn IOPMAssertionCreateWithName(kind: *const NSString, level: u32, name: *const NSString, id: *mut u32) -> i32;
    fn IOPMAssertionRelease(id: u32) -> i32;
}

/// Keeps the Mac from going to sleep on its own while held. The screen may still turn off.
#[derive(Default)]
pub struct Awake(Option<u32>);

impl Awake {
    pub fn hold(&mut self, on: bool) {
        match (on, self.0) {
            (true, None) => {
                let kind = NSString::from_str("PreventUserIdleSystemSleep");
                let name = NSString::from_str("RustTorrent is downloading or uploading");
                let mut id = 0;
                // 255 is "on".
                if unsafe { IOPMAssertionCreateWithName(&*kind, 255, &*name, &mut id) } == 0 {
                    self.0 = Some(id);
                }
            }
            (false, Some(id)) => {
                unsafe { IOPMAssertionRelease(id) };
                self.0 = None;
            }
            _ => {}
        }
    }
}

impl Drop for Awake {
    fn drop(&mut self) {
        self.hold(false);
    }
}
