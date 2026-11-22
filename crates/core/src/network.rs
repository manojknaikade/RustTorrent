//! Network settings that need a new engine session to take effect: the listening port,
//! port forwarding, DHT, local peer discovery, the SOCKS5 proxy and the IP block list
//! (with banned peers).
//!
//! The worker compares these settings with the ones its session was started with, and
//! starts a new session when they differ, the same way it does for the network interface.
use std::net::{IpAddr, Ipv6Addr, SocketAddr};
use std::path::{Path, PathBuf};

use anyhow::Context;
use librqbit::{ConnectionOptions, ListenerMode, ListenerOptions};
use serde::{Deserialize, Serialize};

use crate::blocklist;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(default)]
pub struct NetworkSettings {
    /// The port peers connect to. 0: any free port, a new one each time the engine starts.
    pub port: u16,
    /// Ask the router to forward the port (UPnP).
    pub upnp: bool,
    /// Find peers through the DHT network.
    pub dht: bool,
    /// Find peers on the same local network.
    pub local_discovery: bool,
    /// Also connect to peers over uTP (on UDP), next to TCP. Off by default: the engine
    /// marks it as not yet stable. Never used while the proxy is on, as SOCKS5 here
    /// carries TCP only.
    pub utp: bool,
    pub proxy: ProxySettings,
    /// A block list file the user chose. Read each time the engine starts.
    pub blocklist: Option<PathBuf>,
    /// Addresses of peers banned by hand.
    pub banned: Vec<String>,
}

impl Default for NetworkSettings {
    fn default() -> Self {
        Self {
            port: 0,
            upnp: true,
            dht: true,
            local_discovery: true,
            utp: false,
            proxy: ProxySettings::default(),
            blocklist: None,
            banned: Vec::new(),
        }
    }
}

/// A SOCKS5 proxy for torrent traffic.
///
/// The engine needs the password each time it starts, so it is kept where it can be read
/// back: in the Keychain on macOS (see [`crate::secrets`]), otherwise in the settings file.
/// It is never written to the log.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
#[serde(default)]
pub struct ProxySettings {
    pub enabled: bool,
    pub host: String,
    pub port: u16,
    pub user: String,
    #[serde(skip_serializing_if = "crate::secrets::in_keychain")]
    pub password: String,
}

/// Characters a proxy user name or password may use. The engine hands the password to the
/// proxy exactly as it appears in its proxy address, so only characters that are written
/// the same way in an address are allowed.
fn plain_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || "-._~!$&'()*+,".contains(c)
}

impl ProxySettings {
    /// What is wrong with the proxy settings, if the proxy is on.
    pub fn problem(&self) -> Option<&'static str> {
        if !self.enabled {
            return None;
        }
        let host = self.host.trim();
        if host.is_empty() {
            return Some("The proxy needs an address.");
        }
        let name_ok = host.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-');
        if !name_ok && host.parse::<Ipv6Addr>().is_err() {
            return Some("The proxy address can only be a name or an IP address.");
        }
        if self.port == 0 {
            return Some("The proxy needs a port.");
        }
        if self.user.is_empty() != self.password.is_empty() {
            return Some("Give both a proxy user name and password, or neither.");
        }
        if !self.user.chars().chain(self.password.chars()).all(plain_char) {
            return Some("The proxy user name and password can only use letters, digits and - . _ ~ ! $ & ' ( ) * + ,");
        }
        None
    }

    /// The proxy's address in the form the engine takes, or `None` if the proxy is off.
    /// Contains the password: never log it.
    pub(crate) fn url(&self) -> Option<String> {
        if !self.enabled || self.problem().is_some() {
            return None;
        }
        let host = self.host.trim();
        let host = if host.contains(':') {
            format!("[{host}]")
        } else {
            host.to_owned()
        };
        let login = if self.user.is_empty() {
            String::new()
        } else {
            format!("{}:{}@", self.user, self.password)
        };
        // socks5h: names (of web trackers, feeds) are looked up by the proxy, not here.
        Some(format!("socks5h://{login}{host}:{}", self.port))
    }
}

/// The engine does not tie its connections to the proxy to the chosen interface, so the
/// two together would let traffic leave some other way. Refused rather than allowed.
pub const BOTH: &str = "A proxy cannot be used together with a chosen network interface: the engine would reach the proxy outside that interface.";

/// Takes the proxy password out of a text before it is shown or written to the log.
pub(crate) fn redact(text: &str, settings: &NetworkSettings) -> String {
    let password = &settings.proxy.password;
    if password.is_empty() {
        text.to_owned()
    } else {
        text.replace(password.as_str(), "•••")
    }
}

/// Tidies settings sent by a front end before they are saved.
pub(crate) fn clean(mut settings: NetworkSettings) -> NetworkSettings {
    settings.proxy.host = settings.proxy.host.trim().to_owned();
    settings.blocklist = settings.blocklist.filter(|p| !p.as_os_str().is_empty());
    let mut banned: Vec<String> = Vec::new();
    for text in &settings.banned {
        if let Some(ip) = blocklist::ip(text) {
            let ip = ip.to_string();
            if !banned.contains(&ip) {
                banned.push(ip);
            }
        }
    }
    settings.banned = banned;
    settings
}

/// The address of a peer as the Peers tab shows it (`1.2.3.4:5678`, `[2001:db8::1]:5678`),
/// or a bare address.
pub fn peer_ip(addr: &str) -> Option<IpAddr> {
    match addr.trim().parse::<SocketAddr>() {
        Ok(a) => Some(a.ip().to_canonical()),
        Err(_) => blocklist::ip(addr),
    }
}

/// Adds the peer at `addr` to the banned addresses. Says what happened.
pub(crate) fn ban(settings: &mut NetworkSettings, addr: &str) -> Result<String, String> {
    let ip = peer_ip(addr).ok_or_else(|| format!("{addr} is not a peer address."))?;
    let text = ip.to_string();
    if settings.banned.contains(&text) {
        return Err(format!("{text} is already banned."));
    }
    settings.banned.push(text.clone());
    Ok(text)
}

/// What the running engine was started with, to show in Settings.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NetworkStatus {
    /// The port peers connect to right now. `None` while the engine is not running.
    pub port: Option<u16>,
    /// How many address ranges are blocked, banned peers included.
    pub blocked_ranges: usize,
    /// Lines of the block list file that could not be read.
    pub blocklist_bad_lines: usize,
}

/// The parts of the engine's options that come from the network settings.
pub(crate) struct Prepared {
    pub listen: ListenerOptions,
    pub dht: Option<librqbit::DhtSessionConfig>,
    pub disable_local_service_discovery: bool,
    pub connect: Option<ConnectionOptions>,
    pub blocklist_url: Option<String>,
    pub status: NetworkStatus,
}

/// Turns the settings into engine options. `bind` is the chosen network interface.
pub(crate) async fn prepare(
    settings: &NetworkSettings,
    bind: Option<&str>,
    data_dir: &Path,
) -> anyhow::Result<Prepared> {
    if let Some(problem) = settings.proxy.problem() {
        anyhow::bail!("{problem}");
    }
    if settings.proxy.enabled && bind.is_some() {
        anyhow::bail!("{BOTH}");
    }
    let listen = ListenerOptions {
        mode: if settings.utp && !settings.proxy.enabled {
            ListenerMode::TcpAndUtp
        } else {
            ListenerMode::TcpOnly
        },
        listen_addr: (Ipv6Addr::UNSPECIFIED, settings.port).into(),
        enable_upnp_port_forwarding: settings.upnp,
        ..Default::default()
    };
    let connect = settings.proxy.url().map(|proxy_url| ConnectionOptions {
        proxy_url: Some(proxy_url),
        ..Default::default()
    });
    let (blocklist_url, list) = {
        let settings = settings.clone();
        let data_dir = data_dir.to_owned();
        tokio::task::spawn_blocking(move || write_blocklist(&settings, &data_dir)).await??
    };
    Ok(Prepared {
        listen,
        dht: settings.dht.then(|| dht_config(data_dir)),
        disable_local_service_discovery: !settings.local_discovery,
        connect,
        blocklist_url,
        status: NetworkStatus {
            port: None,
            blocked_ranges: list.len(),
            blocklist_bad_lines: list.bad_lines,
        },
    })
}

/// The engine's DHT options. The engine keeps what the DHT learned, its port among it, in
/// one file per computer user by default, so two copies of the app would both try that
/// port. Keeping the file in the app's own data folder gives each copy its own.
fn dht_config(data_dir: &Path) -> librqbit::DhtSessionConfig {
    librqbit::DhtSessionConfig {
        persistence: Some(librqbit::dht::DhtPersistenceConfig {
            config_filename: Some(data_dir.join(DHT_FILE)),
            ..Default::default()
        }),
        ..Default::default()
    }
}

/// The DHT state file, in the data folder.
const DHT_FILE: &str = "dht.json";

/// Reads the chosen block list, adds the banned peers and writes the result where the
/// engine reads it. No file and no address for it when nothing is blocked.
fn write_blocklist(settings: &NetworkSettings, data_dir: &Path) -> anyhow::Result<(Option<String>, blocklist::List)> {
    let mut list = blocklist::List::default();
    if let Some(path) = &settings.blocklist {
        let bytes =
            std::fs::read(path).with_context(|| format!("the IP block list {} could not be read", path.display()))?;
        if bytes.starts_with(&[0x1f, 0x8b]) || bytes.starts_with(b"PK") {
            anyhow::bail!(
                "the IP block list {} is compressed. Unpack it and choose the text file inside.",
                path.display()
            );
        }
        let from_file = blocklist::parse(&String::from_utf8_lossy(&bytes));
        if from_file.is_empty() {
            anyhow::bail!("no addresses were found in the IP block list {}", path.display());
        }
        list.extend(from_file);
    }
    for ip in settings.banned.iter().filter_map(|t| blocklist::ip(t)) {
        list.add(ip);
    }
    list.tidy();
    let path = data_dir.join("blocklist.txt");
    if list.is_empty() {
        let _ = std::fs::remove_file(&path);
        return Ok((None, list));
    }
    std::fs::write(&path, blocklist::render(&list)).with_context(|| format!("could not write {}", path.display()))?;
    let url = reqwest::Url::from_file_path(&path)
        .map_err(|()| anyhow::anyhow!("could not make an address for {}", path.display()))?;
    Ok((Some(url.to_string()), list))
}

#[cfg(test)]
mod tests {
    use super::*;
    use librqbit::SessionOptions;

    fn proxy() -> ProxySettings {
        ProxySettings {
            enabled: true,
            host: "127.0.0.1".into(),
            port: 1080,
            user: String::new(),
            password: String::new(),
        }
    }

    #[test]
    fn old_settings_files_keep_everything_on() {
        let s: NetworkSettings = serde_json::from_str("{}").unwrap();
        assert_eq!(s, NetworkSettings::default());
        assert!(s.upnp && s.dht && s.local_discovery);
        assert_eq!(s.port, 0);
        assert!(!s.proxy.enabled);
    }

    #[test]
    fn dht_state_lives_in_the_data_folder() {
        let dir = PathBuf::from("/data/a");
        let config = dht_config(&dir);
        let file = config.persistence.and_then(|p| p.config_filename);
        assert_eq!(file, Some(dir.join("dht.json")));
    }

    #[test]
    fn proxy_urls() {
        assert_eq!(proxy().url().as_deref(), Some("socks5h://127.0.0.1:1080"));
        let p = ProxySettings {
            host: "::1".into(),
            user: "me".into(),
            password: "s3cret!".into(),
            ..proxy()
        };
        assert_eq!(p.url().as_deref(), Some("socks5h://me:s3cret!@[::1]:1080"));
        assert_eq!(ProxySettings { enabled: false, ..p }.url(), None);
    }

    #[test]
    fn proxy_problems() {
        assert!(proxy().problem().is_none());
        assert!(ProxySettings {
            host: " ".into(),
            ..proxy()
        }
        .problem()
        .is_some());
        assert!(ProxySettings {
            host: "a b".into(),
            ..proxy()
        }
        .problem()
        .is_some());
        assert!(ProxySettings { port: 0, ..proxy() }.problem().is_some());
        assert!(ProxySettings {
            user: "me".into(),
            ..proxy()
        }
        .problem()
        .is_some());
        let odd = ProxySettings {
            user: "me".into(),
            password: "a@b".into(),
            ..proxy()
        };
        assert!(odd.problem().is_some());
        assert_eq!(odd.url(), None);
        // Problems do not matter while the proxy is off.
        assert!(ProxySettings { enabled: false, ..odd }.problem().is_none());
    }

    #[test]
    fn the_password_is_taken_out_of_messages() {
        let s = NetworkSettings {
            proxy: ProxySettings {
                user: "me".into(),
                password: "hunter22".into(),
                ..proxy()
            },
            ..Default::default()
        };
        let url = s.proxy.url().unwrap();
        let text = redact(&format!("error parsing proxy url {url}"), &s);
        assert!(!text.contains("hunter22"));
        assert!(text.contains("me:•••@127.0.0.1"));
    }

    #[test]
    fn peers_are_banned_by_address_once() {
        let mut s = NetworkSettings::default();
        assert_eq!(ban(&mut s, "1.2.3.4:6881"), Ok("1.2.3.4".into()));
        assert!(ban(&mut s, "1.2.3.4:51413").is_err());
        assert_eq!(ban(&mut s, "[::ffff:5.6.7.8]:1"), Ok("5.6.7.8".into()));
        assert_eq!(ban(&mut s, "[2001:db8::1]:1"), Ok("2001:db8::1".into()));
        assert!(ban(&mut s, "nonsense").is_err());
        assert_eq!(s.banned, ["1.2.3.4", "5.6.7.8", "2001:db8::1"]);
    }

    #[test]
    fn clean_drops_bad_and_repeated_bans() {
        let s = clean(NetworkSettings {
            banned: vec!["1.2.3.4".into(), "x".into(), "001.002.003.004".into()],
            ..Default::default()
        });
        assert_eq!(s.banned, ["1.2.3.4"]);
    }

    #[test]
    fn block_list_file_and_bans_are_written_for_the_engine() {
        let dir = std::env::temp_dir().join(format!("rusttorrent-network-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        // Nothing blocked: no file.
        let (url, list) = write_blocklist(&NetworkSettings::default(), &dir).unwrap();
        assert!(url.is_none() && list.is_empty());

        let file = dir.join("my list.txt");
        std::fs::write(&file, "# list\nbad:10.0.0.0-10.0.0.255\n192.168.0.0/16\nrubbish\n").unwrap();
        let s = NetworkSettings {
            blocklist: Some(file.clone()),
            banned: vec!["8.8.8.8".into()],
            ..Default::default()
        };
        let (url, list) = write_blocklist(&s, &dir).unwrap();
        assert_eq!(list.len(), 3);
        assert_eq!(list.bad_lines, 1);
        let url = url.unwrap();
        assert!(url.starts_with("file://"));
        let written = reqwest::Url::parse(&url).unwrap().to_file_path().unwrap();
        let again = blocklist::parse(&std::fs::read_to_string(written).unwrap());
        assert!(again.contains("8.8.8.8".parse().unwrap()));
        assert!(again.contains("192.168.4.4".parse().unwrap()));

        // A file with no addresses, or a compressed one, stops the engine from starting.
        std::fs::write(&file, "nothing here\n").unwrap();
        assert!(write_blocklist(&s, &dir).is_err());
        std::fs::write(&file, [0x1f, 0x8b, 0, 0]).unwrap();
        assert!(write_blocklist(&s, &dir).is_err());
        std::fs::remove_file(&file).unwrap();
        assert!(write_blocklist(&s, &dir).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_proxy_and_an_interface_are_refused_together() {
        let s = NetworkSettings {
            proxy: proxy(),
            ..Default::default()
        };
        let dir = std::env::temp_dir();
        let e = prepare(&s, Some("utun9"), &dir).await.err().unwrap();
        assert_eq!(e.to_string(), BOTH);
    }

    #[tokio::test]
    async fn utp_is_used_only_when_on_and_there_is_no_proxy() {
        let dir = std::env::temp_dir();
        let utp = |s: NetworkSettings| {
            let dir = dir.clone();
            async move { prepare(&s, None, &dir).await.unwrap().listen.mode.utp_enabled() }
        };
        assert!(!utp(NetworkSettings::default()).await);
        let on = NetworkSettings {
            utp: true,
            ..Default::default()
        };
        assert!(utp(on.clone()).await);
        assert!(!utp(NetworkSettings { proxy: proxy(), ..on }).await);
    }

    /// A tiny SOCKS5 server that answers one connection: it records the login and the
    /// address the engine asks for, then hangs up.
    async fn socks_server() -> (u16, tokio::sync::oneshot::Receiver<(String, String, SocketAddr)>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let (tx, rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let (mut s, _) = listener.accept().await.unwrap();
            let mut head = [0u8; 2];
            s.read_exact(&mut head).await.unwrap();
            let mut methods = vec![0u8; head[1] as usize];
            s.read_exact(&mut methods).await.unwrap();
            assert!(methods.contains(&2), "the engine offers user name and password");
            s.write_all(&[5, 2]).await.unwrap();
            let text = |len: u8| vec![0u8; len as usize];
            let mut b = [0u8; 2];
            s.read_exact(&mut b).await.unwrap();
            let mut user = text(b[1]);
            s.read_exact(&mut user).await.unwrap();
            let mut l = [0u8; 1];
            s.read_exact(&mut l).await.unwrap();
            let mut pass = text(l[0]);
            s.read_exact(&mut pass).await.unwrap();
            s.write_all(&[1, 0]).await.unwrap();
            let mut req = [0u8; 4];
            s.read_exact(&mut req).await.unwrap();
            assert_eq!(req[3], 1, "an IPv4 peer address");
            let mut a = [0u8; 6];
            s.read_exact(&mut a).await.unwrap();
            let target = SocketAddr::from(([a[0], a[1], a[2], a[3]], u16::from_be_bytes([a[4], a[5]])));
            let _ = tx.send((
                String::from_utf8(user).unwrap(),
                String::from_utf8(pass).unwrap(),
                target,
            ));
        });
        (port, rx)
    }

    /// Starts a real engine session with these options and checks that it uses the
    /// chosen port and sends peer connections through the proxy with the login.
    #[tokio::test]
    async fn the_engine_uses_the_port_and_the_proxy() {
        let dir = std::env::temp_dir().join(format!("rusttorrent-proxy-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let free = std::net::TcpListener::bind("[::]:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let (proxy_port, seen) = socks_server().await;
        let s = NetworkSettings {
            port: free,
            upnp: false,
            dht: false,
            local_discovery: false,
            proxy: ProxySettings {
                port: proxy_port,
                user: "me".into(),
                password: "pa$$word".into(),
                ..proxy()
            },
            banned: vec!["10.9.9.9".into()],
            ..Default::default()
        };
        let p = prepare(&s, None, &dir).await.unwrap();
        assert_eq!(p.status.blocked_ranges, 1);
        crate::use_ring();
        let session = librqbit::Session::new_with_opts(
            dir.clone(),
            SessionOptions {
                listen: Some(p.listen),
                dht: p.dht,
                disable_local_service_discovery: p.disable_local_service_discovery,
                connect: p.connect,
                blocklist_url: p.blocklist_url,
                persistence: None,
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(session.listen_addr().map(|a| a.port()), Some(free));

        let peer: SocketAddr = "192.0.2.7:6881".parse().unwrap();
        let magnet = "magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567";
        let add = {
            let session = session.clone();
            tokio::spawn(async move {
                let _ = session
                    .add_torrent(
                        librqbit::AddTorrent::from_url(magnet),
                        Some(librqbit::AddTorrentOptions {
                            initial_peers: Some(vec![peer]),
                            output_folder: Some(dir.to_string_lossy().into_owned()),
                            ..Default::default()
                        }),
                    )
                    .await;
            })
        };
        let (user, pass, target) = tokio::time::timeout(std::time::Duration::from_secs(20), seen)
            .await
            .expect("the engine reached the proxy")
            .unwrap();
        assert_eq!((user.as_str(), pass.as_str(), target), ("me", "pa$$word", peer));
        add.abort();
        session.stop().await;
        let _ = std::fs::remove_dir_all(std::env::temp_dir().join(format!("rusttorrent-proxy-{}", std::process::id())));
    }
}
