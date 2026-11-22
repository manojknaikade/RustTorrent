//! NAT-PMP (RFC 6886): asks the router to forward the listening port, for routers that do
//! not speak UPnP, such as many Apple and ISP routers and some VPNs.
//!
//! The engine does UPnP itself. This runs next to it: a task that maps the port on the
//! default gateway (of the chosen interface, if there is one), renews the mapping halfway
//! through its lifetime and removes it when the engine stops. A router without NAT-PMP
//! does not answer; that is not an error, and it is tried again a few minutes later.
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::net::UdpSocket;

/// The port routers listen on for NAT-PMP.
const PORT: u16 = 5351;
/// How long a mapping is asked for. The RFC suggests two hours.
const LIFETIME: u32 = 7200;
/// How long to wait before trying again when the router did not answer.
const RETRY: Duration = Duration::from_secs(300);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Proto {
    Udp = 1,
    Tcp = 2,
}

/// A mapping request. `lifetime` 0 removes the mapping.
pub(crate) fn request(proto: Proto, port: u16, lifetime: u32) -> [u8; 12] {
    let mut out = [0u8; 12];
    out[1] = proto as u8;
    out[4..6].copy_from_slice(&port.to_be_bytes());
    // Ask for the same port outside; the router may give another.
    out[6..8].copy_from_slice(&if lifetime == 0 { 0 } else { port }.to_be_bytes());
    out[8..12].copy_from_slice(&lifetime.to_be_bytes());
    out
}

/// What the router answered to a mapping request.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Mapped {
    pub external_port: u16,
    pub lifetime: u32,
}

/// Reads the router's answer to a mapping request for `proto` and `port`.
pub(crate) fn answer(bytes: &[u8], proto: Proto, port: u16) -> Result<Mapped, String> {
    if bytes.len() < 16 || bytes[0] != 0 || bytes[1] != 128 + proto as u8 {
        return Err("the router sent an answer that is not NAT-PMP".into());
    }
    let code = u16::from_be_bytes([bytes[2], bytes[3]]);
    if code != 0 {
        return Err(match code {
            1 => "the router does not support this NAT-PMP version".into(),
            2 => "port forwarding is turned off on the router".into(),
            3 => "the router is not connected to the internet".into(),
            4 => "the router has no free ports".into(),
            n => format!("the router refused (code {n})"),
        });
    }
    if u16::from_be_bytes([bytes[8], bytes[9]]) != port {
        return Err("the router answered for another port".into());
    }
    Ok(Mapped {
        external_port: u16::from_be_bytes([bytes[10], bytes[11]]),
        lifetime: u32::from_be_bytes([bytes[12], bytes[13], bytes[14], bytes[15]]),
    })
}

/// Reads the router's public address from its answer to opcode 0.
pub(crate) fn public_address(bytes: &[u8]) -> Option<Ipv4Addr> {
    if bytes.len() < 12 || bytes[0] != 0 || bytes[1] != 128 || bytes[2..4] != [0, 0] {
        return None;
    }
    Some(Ipv4Addr::new(bytes[8], bytes[9], bytes[10], bytes[11]))
}

/// Sends `message` and waits for an answer, resending as the RFC says: after 250 ms,
/// then twice as long each time. Gives up after about 4 s, as most routers without
/// NAT-PMP never answer.
async fn ask(socket: &UdpSocket, to: SocketAddrV4, message: &[u8], check: impl Fn(&[u8]) -> bool) -> Option<Vec<u8>> {
    let gateway = *to.ip();
    let to = SocketAddr::V4(to);
    let mut wait = Duration::from_millis(250);
    let mut buf = [0u8; 64];
    for _ in 0..5 {
        socket.send_to(message, to).await.ok()?;
        let deadline = tokio::time::Instant::now() + wait;
        loop {
            match tokio::time::timeout_at(deadline, socket.recv_from(&mut buf)).await {
                Ok(Ok((n, from))) if from.ip() == std::net::IpAddr::V4(gateway) && check(&buf[..n]) => {
                    return Some(buf[..n].to_vec());
                }
                Ok(Ok(_)) => continue,
                Ok(Err(_)) => return None,
                Err(_) => break,
            }
        }
        wait *= 2;
    }
    None
}

/// The default gateway's IPv4 address, through `interface` when one is chosen.
#[cfg(target_os = "macos")]
pub(crate) fn gateway(interface: Option<&str>) -> Option<Ipv4Addr> {
    let mut cmd = std::process::Command::new("/sbin/route");
    cmd.args(["-n", "get"]);
    if let Some(name) = interface {
        cmd.args(["-ifscope", name]);
    }
    let out = cmd.arg("default").output().ok()?;
    parse_route(&String::from_utf8_lossy(&out.stdout))
}

/// Reads the `gateway:` line of `route -n get default`.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn parse_route(text: &str) -> Option<Ipv4Addr> {
    text.lines()
        .find_map(|l| l.trim().strip_prefix("gateway:"))
        .and_then(|g| g.trim().parse().ok())
}

#[cfg(target_os = "linux")]
pub(crate) fn gateway(interface: Option<&str>) -> Option<Ipv4Addr> {
    parse_proc_route(&std::fs::read_to_string("/proc/net/route").ok()?, interface)
}

/// Reads the default route from `/proc/net/route`, where addresses are little-endian hex.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn parse_proc_route(text: &str, interface: Option<&str>) -> Option<Ipv4Addr> {
    text.lines().skip(1).find_map(|line| {
        let f: Vec<&str> = line.split_whitespace().collect();
        if f.len() < 3 || f[1] != "00000000" || interface.is_some_and(|i| i != f[0]) {
            return None;
        }
        let g = u32::from_str_radix(f[2], 16).ok()?;
        (g != 0).then(|| Ipv4Addr::from(g.swap_bytes()))
    })
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub(crate) fn gateway(_interface: Option<&str>) -> Option<Ipv4Addr> {
    None
}

/// What the mapping looks like right now, for Settings.
pub type State = Arc<Mutex<Option<String>>>;

/// Keeps the port mapped while it lives. [`Mapper::stop`] removes the mapping.
pub(crate) struct Mapper {
    stop: tokio::sync::oneshot::Sender<()>,
    task: tokio::task::JoinHandle<()>,
    pub state: State,
}

impl Mapper {
    /// Starts mapping `port` over TCP, and over UDP too when `udp` (uTP) is on. `bind` is
    /// the chosen interface and its IPv4 address.
    pub fn start(port: u16, udp: bool, bind: Option<(String, Ipv4Addr)>) -> Self {
        let (stop, mut stopped) = tokio::sync::oneshot::channel();
        let state: State = Arc::default();
        let shown = state.clone();
        let mut protos = vec![Proto::Tcp];
        if udp {
            protos.push(Proto::Udp);
        }
        let task = tokio::spawn(async move {
            let set = |text: Option<String>| *shown.lock().unwrap_or_else(|e| e.into_inner()) = text;
            let mut mapped_on: Option<(UdpSocket, SocketAddrV4)> = None;
            loop {
                let wait = match map_once(port, &protos, bind.as_ref()).await {
                    Ok((socket, gateway, text, lifetime)) => {
                        set(Some(text));
                        mapped_on = Some((socket, gateway));
                        Duration::from_secs(u64::from(lifetime.max(120) / 2))
                    }
                    Err(text) => {
                        set(text);
                        RETRY
                    }
                };
                tokio::select! {
                    _ = tokio::time::sleep(wait) => {}
                    _ = &mut stopped => break,
                }
            }
            if let Some((socket, gateway)) = mapped_on {
                for &proto in &protos {
                    let msg = request(proto, port, 0);
                    let _ = tokio::time::timeout(
                        Duration::from_secs(1),
                        ask(&socket, gateway, &msg, |b| answer(b, proto, port).is_ok()),
                    )
                    .await;
                }
            }
        });
        Self { stop, task, state }
    }

    /// Removes the mapping, waiting at most a second or two for the router.
    pub async fn stop(self) {
        let _ = self.stop.send(());
        let _ = tokio::time::timeout(Duration::from_secs(3), self.task).await;
    }
}

/// Maps the port once. On success: the socket and gateway used, a line for Settings and
/// the shortest lifetime given. On failure: a line for Settings, or `None` when the
/// router simply does not speak NAT-PMP.
async fn map_once(
    port: u16,
    protos: &[Proto],
    bind: Option<&(String, Ipv4Addr)>,
) -> Result<(UdpSocket, SocketAddrV4, String, u32), Option<String>> {
    let iface = bind.map(|(name, _)| name.clone());
    let gateway = tokio::task::spawn_blocking(move || gateway(iface.as_deref()))
        .await
        .ok()
        .flatten()
        .ok_or(None)?;
    let local = bind.map_or(Ipv4Addr::UNSPECIFIED, |(_, ip)| *ip);
    let socket = UdpSocket::bind((local, 0)).await.map_err(|_| None)?;
    let gateway = SocketAddrV4::new(gateway, PORT);
    let (text, lifetime) = map_on(&socket, gateway, port, protos).await?;
    Ok((socket, gateway, text, lifetime))
}

/// Asks the router at `gateway` for its public address, then maps `port` for each of
/// `protos`. Returns a line for Settings and the shortest lifetime given.
async fn map_on(
    socket: &UdpSocket,
    gateway: SocketAddrV4,
    port: u16,
    protos: &[Proto],
) -> Result<(String, u32), Option<String>> {
    let public = ask(socket, gateway, &[0, 0], |b| public_address(b).is_some())
        .await
        .and_then(|b| public_address(&b));
    let Some(public) = public else {
        return Err(None);
    };
    let mut external = port;
    let mut lifetime = LIFETIME;
    for &proto in protos {
        let reply = ask(socket, gateway, &request(proto, port, LIFETIME), |b| {
            b.len() >= 2 && b[1] == 128 + proto as u8
        })
        .await
        .ok_or(None)?;
        let m = answer(&reply, proto, port).map_err(|e| Some(format!("NAT-PMP: {e}.")))?;
        if proto == Proto::Tcp {
            external = m.external_port;
        }
        lifetime = lifetime.min(m.lifetime);
    }
    let text = if external == port {
        format!("The router forwards port {port} (NAT-PMP). Public address: {public}.")
    } else {
        format!("The router forwards its port {external} to {port} (NAT-PMP). Public address: {public}.")
    };
    Ok((text, lifetime))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_follow_the_rfc() {
        assert_eq!(
            request(Proto::Tcp, 51413, 7200),
            [0, 2, 0, 0, 0xc8, 0xd5, 0xc8, 0xd5, 0, 0, 0x1c, 0x20]
        );
        // Removing: lifetime 0 and no suggested port.
        assert_eq!(
            request(Proto::Udp, 51413, 0),
            [0, 1, 0, 0, 0xc8, 0xd5, 0, 0, 0, 0, 0, 0]
        );
    }

    #[test]
    fn reads_answers() {
        let mut ok = [0u8; 16];
        ok[1] = 130;
        ok[8..10].copy_from_slice(&51413u16.to_be_bytes());
        ok[10..12].copy_from_slice(&40000u16.to_be_bytes());
        ok[12..16].copy_from_slice(&3600u32.to_be_bytes());
        assert_eq!(
            answer(&ok, Proto::Tcp, 51413),
            Ok(Mapped {
                external_port: 40000,
                lifetime: 3600
            })
        );
        assert!(answer(&ok, Proto::Udp, 51413).is_err());
        assert!(answer(&ok, Proto::Tcp, 1).is_err());
        let mut refused = ok;
        refused[3] = 2;
        assert_eq!(
            answer(&refused, Proto::Tcp, 51413).unwrap_err(),
            "port forwarding is turned off on the router"
        );
        assert!(answer(&ok[..10], Proto::Tcp, 51413).is_err());

        let public = [0, 128, 0, 0, 0, 0, 0, 1, 203, 0, 113, 7];
        assert_eq!(public_address(&public), Some(Ipv4Addr::new(203, 0, 113, 7)));
        assert_eq!(public_address(&public[..8]), None);
    }

    #[test]
    fn finds_the_gateway_in_route_output() {
        let mac = "   route to: default\ndestination: default\n       mask: default\n    gateway: 192.168.1.1\n  interface: en0\n";
        assert_eq!(parse_route(mac), Some(Ipv4Addr::new(192, 168, 1, 1)));
        assert_eq!(parse_route("gateway: fe80::1%en0\n"), None);

        let linux = "Iface\tDestination\tGateway\tFlags\n\
                     wg0\t0000000A\t00000000\t0001\n\
                     eth0\t00000000\t0101A8C0\t0003\n\
                     wg0\t00000000\t0102000A\t0003\n";
        assert_eq!(parse_proc_route(linux, None), Some(Ipv4Addr::new(192, 168, 1, 1)));
        assert_eq!(parse_proc_route(linux, Some("wg0")), Some(Ipv4Addr::new(10, 0, 2, 1)));
        assert_eq!(parse_proc_route(linux, Some("eth1")), None);
    }

    /// A fake router on the loopback answers the way a real one does.
    #[tokio::test]
    async fn maps_through_a_router_that_answers() {
        let router = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let at = router.local_addr().unwrap().port();
        tokio::spawn(async move {
            let mut buf = [0u8; 64];
            loop {
                let (n, from) = router.recv_from(&mut buf).await.unwrap();
                let reply: Vec<u8> = if n == 2 {
                    vec![0, 128, 0, 0, 0, 0, 0, 1, 198, 51, 100, 2]
                } else {
                    let mut r = vec![0, 128 + buf[1], 0, 0, 0, 0, 0, 1];
                    r.extend_from_slice(&buf[4..6]);
                    r.extend_from_slice(&buf[6..8]);
                    r.extend_from_slice(&buf[8..12]);
                    r
                };
                router.send_to(&reply, from).await.unwrap();
            }
        });
        let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let gateway = SocketAddrV4::new(Ipv4Addr::LOCALHOST, at);
        let (text, lifetime) = map_on(&socket, gateway, 6881, &[Proto::Tcp, Proto::Udp]).await.unwrap();
        assert_eq!(
            text,
            "The router forwards port 6881 (NAT-PMP). Public address: 198.51.100.2."
        );
        assert_eq!(lifetime, LIFETIME);
    }
}
