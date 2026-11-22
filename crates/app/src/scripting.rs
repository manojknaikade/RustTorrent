//! AppleScript support. The commands are declared in `assets/RustTorrent.sdef`, which the
//! bundle names in its Info.plist, and arrive here as plain Apple Events. The plist also sets
//! NSAppleScriptEnabled, without which Script Editor will not load the terms, but the commands
//! have no Cocoa scripting classes: the handlers below take their events instead, since an
//! Objective-C object model for the torrents would be a lot of code for four commands.
//!
//! ```applescript
//! tell application "RustTorrent"
//!     add torrent "magnet:?xt=urn:btih:…"
//!     pause torrents {"ubuntu-24.04.iso"}
//!     resume torrents
//!     get torrent list
//! end tell
//! ```
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{class, define_class, msg_send, sel, AllocAnyThread};
use objc2_foundation::{NSObject, NSString};
use rusttorrent_core::{Command, Remote};
use std::sync::OnceLock;

static REMOTE: OnceLock<Remote> = OnceLock::new();
/// The same file the bundle carries, which tools that read it without running the app use.
const DICTIONARY: &str = include_str!("../../../assets/RustTorrent.sdef");

/// Called once the engine runs. Until then every command answers that the app is starting.
pub fn connect(remote: Remote) {
    let _ = REMOTE.set(remote);
}

const fn code(c: &[u8; 4]) -> u32 {
    u32::from_be_bytes(*c)
}

/// The suite's event class, and the parts of an event and its reply.
const SUITE: u32 = code(b"RsTt");
const DIRECT: u32 = code(b"----");
const ERROR_NUMBER: u32 = code(b"errn");
const ERROR_TEXT: u32 = code(b"errs");
const TYPE_LIST: u32 = code(b"list");
const TYPE_NULL: u32 = code(b"null");

/// "Can't get" — what AppleScript reports for an object that does not exist.
const NO_SUCH_OBJECT: i32 = -1728;
/// A general failure, used while the engine is still starting.
const FAILED: i32 = -10000;

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "RustTorrentScripting"]
    struct Handler;

    impl Handler {
        /// AppleScript asks the running app for its dictionary. Cocoa scripting would answer
        /// this, but it is not on, so the dictionary is sent from here.
        #[unsafe(method(dictionary:withReplyEvent:))]
        fn dictionary(&self, _event: &AnyObject, reply: &AnyObject) {
            let sdef = DICTIONARY.as_bytes();
            unsafe {
                let desc: Retained<AnyObject> = msg_send![
                    class!(NSAppleEventDescriptor),
                    descriptorWithDescriptorType: code(b"utf8"),
                    bytes: sdef.as_ptr().cast::<std::ffi::c_void>(),
                    length: sdef.len()
                ];
                answer(reply, &desc);
            }
        }

        #[unsafe(method(addTorrent:withReplyEvent:))]
        fn add_torrent(&self, event: &AnyObject, reply: &AnyObject) {
            let texts = direct_texts(event);
            if texts.is_empty() {
                fail(reply, FAILED, "Give a magnet link, a web address or the path of a .torrent file.");
                return;
            }
            for text in texts {
                crate::deliver(text);
            }
        }

        #[unsafe(method(pauseTorrents:withReplyEvent:))]
        fn pause_torrents(&self, event: &AnyObject, reply: &AnyObject) {
            if let Some(keys) = chosen(event, reply) {
                send(Command::Pause(keys));
            }
        }

        #[unsafe(method(resumeTorrents:withReplyEvent:))]
        fn resume_torrents(&self, event: &AnyObject, reply: &AnyObject) {
            if let Some(keys) = chosen(event, reply) {
                send(Command::Resume(keys));
            }
        }

        #[unsafe(method(torrentList:withReplyEvent:))]
        fn torrent_list(&self, _event: &AnyObject, reply: &AnyObject) {
            let Some(remote) = REMOTE.get() else {
                fail(reply, FAILED, "RustTorrent is still starting.");
                return;
            };
            let snap = remote.snapshot();
            unsafe {
                let list: Retained<AnyObject> = msg_send![class!(NSAppleEventDescriptor), listDescriptor];
                for t in &snap.torrents {
                    let record: Retained<AnyObject> = msg_send![class!(NSAppleEventDescriptor), recordDescriptor];
                    set(&record, b"pnam", &text(&t.name));
                    set(&record, b"TKey", &text(&t.key));
                    set(&record, b"TSta", &text(t.state.label()));
                    set(&record, b"TPrg", &real(f64::from(t.progress()) * 100.0));
                    set(&record, b"TSiz", &real(t.total as f64));
                    set(&record, b"TDon", &real(t.done as f64));
                    set(&record, b"TDnS", &real(t.down_bps as f64));
                    set(&record, b"TUpS", &real(t.up_bps as f64));
                    set(&record, b"TFol", &text(&t.save_path));
                    let _: () = msg_send![&*list, insertDescriptor: &*record, atIndex: 0isize];
                }
                answer(reply, &list);
            }
        }
    }
);

/// Registers the handlers. Called from the launch observer, before any event can arrive.
pub fn install(manager: *mut AnyObject) {
    let handler: Retained<Handler> = unsafe { msg_send![Handler::alloc(), init] };
    let events = [
        (b"AddT", sel!(addTorrent:withReplyEvent:)),
        (b"Paus", sel!(pauseTorrents:withReplyEvent:)),
        (b"Resm", sel!(resumeTorrents:withReplyEvent:)),
        (b"List", sel!(torrentList:withReplyEvent:)),
    ];
    for (id, selector) in events {
        unsafe {
            let _: () = msg_send![manager, setEventHandler: &*handler, andSelector: selector, forEventClass: SUITE, andEventID: code(id)];
        }
    }
    unsafe {
        let _: () = msg_send![manager, setEventHandler: &*handler, andSelector: sel!(dictionary:withReplyEvent:), forEventClass: code(b"ascr"), andEventID: code(b"gsdf")];
    }
    std::mem::forget(handler); // lives for the whole run
}

fn send(command: Command) {
    if let Some(remote) = REMOTE.get() {
        remote.send(command);
    }
}

/// The torrents a pause or resume names, by name or key, or all of them when it names none.
/// Answers with an error and returns `None` if a name matches nothing.
fn chosen(event: &AnyObject, reply: &AnyObject) -> Option<Vec<String>> {
    let Some(remote) = REMOTE.get() else {
        fail(reply, FAILED, "RustTorrent is still starting.");
        return None;
    };
    let snap = remote.snapshot();
    let wanted = direct_texts(event);
    if wanted.is_empty() {
        return Some(snap.torrents.iter().map(|t| t.key.clone()).collect());
    }
    let mut keys = Vec::new();
    for want in &wanted {
        let found: Vec<String> = snap
            .torrents
            .iter()
            .filter(|t| &t.key == want || t.name.eq_ignore_ascii_case(want) || t.own_name.eq_ignore_ascii_case(want))
            .map(|t| t.key.clone())
            .collect();
        if found.is_empty() {
            fail(reply, NO_SUCH_OBJECT, &format!("There is no torrent called \u{201c}{want}\u{201d}."));
            return None;
        }
        keys.extend(found);
    }
    Some(keys)
}

/// The event's direct parameter as text: one string, or each item of a list.
fn direct_texts(event: &AnyObject) -> Vec<String> {
    unsafe {
        let desc: *mut AnyObject = msg_send![event, paramDescriptorForKeyword: DIRECT];
        let Some(desc) = desc.as_ref() else {
            return Vec::new();
        };
        let kind: u32 = msg_send![desc, descriptorType];
        let items: Vec<&AnyObject> = if kind == TYPE_LIST {
            let n: isize = msg_send![desc, numberOfItems];
            (1..=n)
                .filter_map(|i| {
                    let item: *mut AnyObject = msg_send![desc, descriptorAtIndex: i];
                    item.as_ref()
                })
                .collect()
        } else {
            vec![desc]
        };
        items
            .into_iter()
            .filter_map(|item| {
                let s: *mut NSString = msg_send![item, stringValue];
                s.as_ref().map(|s| s.to_string())
            })
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    }
}

fn text(s: &str) -> Retained<AnyObject> {
    let s = NSString::from_str(s);
    unsafe { msg_send![class!(NSAppleEventDescriptor), descriptorWithString: &*s] }
}

fn real(n: f64) -> Retained<AnyObject> {
    unsafe { msg_send![class!(NSAppleEventDescriptor), descriptorWithDouble: n] }
}

fn set(record: &AnyObject, key: &[u8; 4], value: &AnyObject) {
    unsafe {
        let _: () = msg_send![record, setDescriptor: value, forKeyword: code(key)];
    }
}

/// True when the sender wants an answer. A script that ignores replies sends a null one.
fn wants_reply(reply: &AnyObject) -> bool {
    let kind: u32 = unsafe { msg_send![reply, descriptorType] };
    kind != TYPE_NULL
}

fn answer(reply: &AnyObject, value: &AnyObject) {
    if wants_reply(reply) {
        unsafe {
            let _: () = msg_send![reply, setParamDescriptor: value, forKeyword: DIRECT];
        }
    }
}

fn fail(reply: &AnyObject, number: i32, message: &str) {
    if !wants_reply(reply) {
        return;
    }
    unsafe {
        let n: Retained<AnyObject> = msg_send![class!(NSAppleEventDescriptor), descriptorWithInt32: number];
        let _: () = msg_send![reply, setParamDescriptor: &*n, forKeyword: ERROR_NUMBER];
        let _: () = msg_send![reply, setParamDescriptor: &*text(message), forKeyword: ERROR_TEXT];
    }
}
