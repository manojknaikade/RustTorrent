//! Passwords kept in the macOS Keychain instead of the settings file.
//!
//! Only the proxy password is a secret the app must be able to read back (the remote-control
//! password is stored as a hash). It is off until the app calls [`use_keychain`], so tests
//! and other front ends never touch the user's Keychain. With it on:
//!
//! - the password is left out when the settings file is written;
//! - loading moves a password found in the file into the Keychain, then rewrites the file
//!   without it;
//! - saving updates or deletes the Keychain item when the password changed.
//!
//! If the Keychain refuses, the password goes back to the settings file for the rest of the
//! run, as before, and a notice says why.
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

static ON: AtomicBool = AtomicBool::new(false);
/// The password last written to or read from the Keychain, so a save only touches the
/// Keychain when it changed.
static KEPT: Mutex<Option<String>> = Mutex::new(None);

/// What the Keychain item is called. Its account is the settings file's path, so a second
/// data folder (`RUSTTORRENT_DATA_DIR`) has its own.
const SERVICE: &str = "RustTorrent proxy password";

/// Keep the proxy password in the Keychain from now on. Call before the engine starts.
/// Does nothing on systems without one.
pub fn use_keychain() {
    ON.store(cfg!(target_os = "macos"), Ordering::Relaxed);
}

/// True while the proxy password lives in the Keychain rather than in state.json.
pub fn in_use() -> bool {
    ON.load(Ordering::Relaxed)
}

/// For `skip_serializing_if`: true while the password lives in the Keychain.
pub(crate) fn in_keychain(_password: &str) -> bool {
    ON.load(Ordering::Relaxed)
}

/// Why the Keychain was given up on, waiting to be shown once.
static NOTE: Mutex<Option<String>> = Mutex::new(None);

fn give_up(why: String) {
    ON.store(false, Ordering::Relaxed);
    *NOTE.lock().unwrap_or_else(|e| e.into_inner()) = Some(format!(
        "Keychain: {why}. The proxy password is kept in the settings file instead."
    ));
}

/// A message to show if the Keychain was given up on since the last call.
pub(crate) fn take_note() -> Option<String> {
    NOTE.lock().unwrap_or_else(|e| e.into_inner()).take()
}

/// After the settings file was read. Returns true if the file still holds a password that is
/// now in the Keychain, so the caller should write the file again without it.
pub(crate) fn after_load(state_file: &Path, password: &mut String) -> bool {
    if !ON.load(Ordering::Relaxed) {
        return false;
    }
    let account = state_file.to_string_lossy();
    if !password.is_empty() {
        return match keychain::write(SERVICE, &account, password) {
            Ok(()) => {
                *KEPT.lock().unwrap_or_else(|e| e.into_inner()) = Some(password.clone());
                true
            }
            Err(e) => {
                give_up(e);
                false
            }
        };
    }
    match keychain::read(SERVICE, &account) {
        Ok(found) => {
            *password = found.unwrap_or_default();
            *KEPT.lock().unwrap_or_else(|e| e.into_inner()) = Some(password.clone());
        }
        Err(e) => give_up(e),
    }
    false
}

/// Before the settings file is written: brings the Keychain up to date.
pub(crate) fn before_save(state_file: &Path, password: &str) {
    if !ON.load(Ordering::Relaxed) {
        return;
    }
    let mut kept = KEPT.lock().unwrap_or_else(|e| e.into_inner());
    if kept.as_deref() == Some(password) {
        return;
    }
    let account = state_file.to_string_lossy();
    let done = if password.is_empty() {
        keychain::delete(SERVICE, &account)
    } else {
        keychain::write(SERVICE, &account, password)
    };
    match done {
        Ok(()) => *kept = Some(password.to_owned()),
        Err(e) => give_up(e),
    }
}

#[cfg(target_os = "macos")]
mod keychain {
    //! Generic passwords through Security.framework, by hand: no crate for four calls.
    use std::ffi::c_void;

    type CFTypeRef = *const c_void;
    type OSStatus = i32;

    const NOT_FOUND: OSStatus = -25300;
    const UTF8: u32 = 0x0800_0100;

    #[link(name = "Security", kind = "framework")]
    unsafe extern "C" {
        static kSecClass: CFTypeRef;
        static kSecClassGenericPassword: CFTypeRef;
        static kSecAttrService: CFTypeRef;
        static kSecAttrAccount: CFTypeRef;
        static kSecValueData: CFTypeRef;
        static kSecReturnData: CFTypeRef;
        static kSecMatchLimit: CFTypeRef;
        static kSecMatchLimitOne: CFTypeRef;
        fn SecItemAdd(attributes: CFTypeRef, result: *mut CFTypeRef) -> OSStatus;
        fn SecItemCopyMatching(query: CFTypeRef, result: *mut CFTypeRef) -> OSStatus;
        fn SecItemUpdate(query: CFTypeRef, changes: CFTypeRef) -> OSStatus;
        fn SecItemDelete(query: CFTypeRef) -> OSStatus;
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        static kCFBooleanTrue: CFTypeRef;
        static kCFTypeDictionaryKeyCallBacks: c_void;
        static kCFTypeDictionaryValueCallBacks: c_void;
        fn CFDictionaryCreate(
            allocator: CFTypeRef,
            keys: *const CFTypeRef,
            values: *const CFTypeRef,
            count: isize,
            key_callbacks: *const c_void,
            value_callbacks: *const c_void,
        ) -> CFTypeRef;
        fn CFDataCreate(allocator: CFTypeRef, bytes: *const u8, length: isize) -> CFTypeRef;
        fn CFDataGetLength(data: CFTypeRef) -> isize;
        fn CFDataGetBytePtr(data: CFTypeRef) -> *const u8;
        fn CFStringCreateWithBytes(
            allocator: CFTypeRef,
            bytes: *const u8,
            length: isize,
            encoding: u32,
            external: u8,
        ) -> CFTypeRef;
        fn CFRelease(object: CFTypeRef);
    }

    /// A Core Foundation object released when dropped.
    struct Owned(CFTypeRef);

    impl Drop for Owned {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe { CFRelease(self.0) }
            }
        }
    }

    fn string(s: &str) -> Owned {
        Owned(unsafe { CFStringCreateWithBytes(std::ptr::null(), s.as_ptr(), s.len() as isize, UTF8, 0) })
    }

    fn data(bytes: &[u8]) -> Owned {
        Owned(unsafe { CFDataCreate(std::ptr::null(), bytes.as_ptr(), bytes.len() as isize) })
    }

    fn dictionary(pairs: &[(CFTypeRef, CFTypeRef)]) -> Owned {
        let keys: Vec<CFTypeRef> = pairs.iter().map(|p| p.0).collect();
        let values: Vec<CFTypeRef> = pairs.iter().map(|p| p.1).collect();
        Owned(unsafe {
            CFDictionaryCreate(
                std::ptr::null(),
                keys.as_ptr(),
                values.as_ptr(),
                pairs.len() as isize,
                &kCFTypeDictionaryKeyCallBacks,
                &kCFTypeDictionaryValueCallBacks,
            )
        })
    }

    fn failed(what: &str, status: OSStatus) -> String {
        format!("{what} failed (error {status})")
    }

    pub fn write(service: &str, account: &str, password: &str) -> Result<(), String> {
        let (service, account, value) = (string(service), string(account), data(password.as_bytes()));
        unsafe {
            let item = [
                (kSecClass, kSecClassGenericPassword),
                (kSecAttrService, service.0),
                (kSecAttrAccount, account.0),
            ];
            let query = dictionary(&item);
            let change = dictionary(&[(kSecValueData, value.0)]);
            match SecItemUpdate(query.0, change.0) {
                0 => Ok(()),
                NOT_FOUND => {
                    let mut new = item.to_vec();
                    new.push((kSecValueData, value.0));
                    let new = dictionary(&new);
                    match SecItemAdd(new.0, std::ptr::null_mut()) {
                        0 => Ok(()),
                        status => Err(failed("saving the password", status)),
                    }
                }
                status => Err(failed("updating the password", status)),
            }
        }
    }

    pub fn read(service: &str, account: &str) -> Result<Option<String>, String> {
        let (service, account) = (string(service), string(account));
        unsafe {
            let query = dictionary(&[
                (kSecClass, kSecClassGenericPassword),
                (kSecAttrService, service.0),
                (kSecAttrAccount, account.0),
                (kSecReturnData, kCFBooleanTrue),
                (kSecMatchLimit, kSecMatchLimitOne),
            ]);
            let mut found: CFTypeRef = std::ptr::null();
            match SecItemCopyMatching(query.0, &mut found) {
                0 if !found.is_null() => {
                    let found = Owned(found);
                    let bytes =
                        std::slice::from_raw_parts(CFDataGetBytePtr(found.0), CFDataGetLength(found.0) as usize);
                    String::from_utf8(bytes.to_vec())
                        .map(Some)
                        .map_err(|_| "the saved password is not text".to_owned())
                }
                0 | NOT_FOUND => Ok(None),
                status => Err(failed("reading the password", status)),
            }
        }
    }

    pub fn delete(service: &str, account: &str) -> Result<(), String> {
        let (service, account) = (string(service), string(account));
        unsafe {
            let query = dictionary(&[
                (kSecClass, kSecClassGenericPassword),
                (kSecAttrService, service.0),
                (kSecAttrAccount, account.0),
            ]);
            match SecItemDelete(query.0) {
                0 | NOT_FOUND => Ok(()),
                status => Err(failed("removing the password", status)),
            }
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod keychain {
    pub fn write(_: &str, _: &str, _: &str) -> Result<(), String> {
        Err("there is no Keychain".into())
    }
    pub fn read(_: &str, _: &str) -> Result<Option<String>, String> {
        Err("there is no Keychain".into())
    }
    pub fn delete(_: &str, _: &str) -> Result<(), String> {
        Err("there is no Keychain".into())
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::keychain;

    /// Touches the real Keychain with a throwaway item, so it only runs when asked:
    /// `cargo test -p rusttorrent-core keychain_round_trip -- --ignored`
    #[test]
    #[ignore]
    fn keychain_round_trip() {
        let (service, account) = ("RustTorrent test item", "round trip");
        keychain::delete(service, account).unwrap();
        assert_eq!(keychain::read(service, account).unwrap(), None);
        keychain::write(service, account, "first").unwrap();
        keychain::write(service, account, "sécond").unwrap();
        assert_eq!(keychain::read(service, account).unwrap().as_deref(), Some("sécond"));
        keychain::delete(service, account).unwrap();
        assert_eq!(keychain::read(service, account).unwrap(), None);
    }
}
