//! The machine's network interfaces, for binding all traffic to one of them.

/// An interface that is up and has an address the app could use.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Interface {
    /// The system's name for it, such as `en0` or `utun4`.
    pub name: String,
    /// Its addresses as text, IPv4 first.
    pub addrs: Vec<String>,
}

/// Every interface that is up and has an address, sorted by name. Interfaces that only
/// have a link-local IPv6 address are left out: nothing on the internet is reachable
/// through them, and macOS keeps several idle `utun` interfaces in that state.
#[cfg(unix)]
pub fn interfaces() -> Vec<Interface> {
    use std::ffi::CStr;
    use std::net::{Ipv4Addr, Ipv6Addr};

    let mut found: Vec<(String, bool, String)> = Vec::new();
    let mut list: *mut libc::ifaddrs = std::ptr::null_mut();
    // SAFETY: getifaddrs fills `list` with a linked list that stays valid until
    // freeifaddrs. Every pointer is checked for null before it is read, and an address
    // is only read as the type its family says it is.
    unsafe {
        if libc::getifaddrs(&mut list) != 0 {
            return Vec::new();
        }
        let mut at = list;
        while !at.is_null() {
            let entry = &*at;
            at = entry.ifa_next;
            let wanted = (libc::IFF_UP | libc::IFF_RUNNING) as libc::c_uint;
            if entry.ifa_addr.is_null() || entry.ifa_name.is_null() || entry.ifa_flags & wanted != wanted {
                continue;
            }
            let name = CStr::from_ptr(entry.ifa_name).to_string_lossy().into_owned();
            match i32::from((*entry.ifa_addr).sa_family) {
                libc::AF_INET => {
                    let a = &*(entry.ifa_addr as *const libc::sockaddr_in);
                    let ip = Ipv4Addr::from(u32::from_be(a.sin_addr.s_addr));
                    found.push((name, false, ip.to_string()));
                }
                libc::AF_INET6 => {
                    let a = &*(entry.ifa_addr as *const libc::sockaddr_in6);
                    let ip = Ipv6Addr::from(a.sin6_addr.s6_addr);
                    // fe80::/10 is link-local.
                    if ip.segments()[0] & 0xffc0 != 0xfe80 {
                        found.push((name, true, ip.to_string()));
                    }
                }
                _ => {}
            }
        }
        libc::freeifaddrs(list);
    }
    found.sort();
    let mut out: Vec<Interface> = Vec::new();
    for (name, _, addr) in found {
        match out.last_mut() {
            Some(last) if last.name == name => last.addrs.push(addr),
            _ => out.push(Interface {
                name,
                addrs: vec![addr],
            }),
        }
    }
    out
}

/// The number the system gives the interface right now. It changes when the interface
/// goes away and comes back, which is how a reconnected VPN is noticed.
#[cfg(unix)]
pub fn index(name: &str) -> Option<u32> {
    let name = std::ffi::CString::new(name).ok()?;
    // SAFETY: the pointer is to a string that ends in a zero byte and lives through the call.
    let index = unsafe { libc::if_nametoindex(name.as_ptr()) };
    (index != 0).then_some(index)
}

#[cfg(not(unix))]
pub fn index(_name: &str) -> Option<u32> {
    None
}

/// Listing interfaces is only written for Unix systems so far.
#[cfg(not(unix))]
pub fn interfaces() -> Vec<Interface> {
    Vec::new()
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn lists_the_loopback() {
        let all = interfaces();
        let lo = all
            .iter()
            .find(|i| i.addrs.iter().any(|a| a == "127.0.0.1"))
            .expect("a loopback interface");
        assert!(lo.name.starts_with("lo"));
        assert!(all.windows(2).all(|w| w[0].name < w[1].name));
    }
}
