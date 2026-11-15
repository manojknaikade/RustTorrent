//! Runs the core with no window, for testing by hand.
//!
//! `headless <data dir> [<seconds>:<interface> ...]` prints one line a second. Each
//! `<seconds>:<interface>` chooses that network interface that many seconds after the
//! start; the interface `any` means no choice. It stops ten seconds after the last one.
use std::time::{Duration, Instant};

use rusttorrent_core::{Command, Core};

fn main() {
    let mut args = std::env::args().skip(1);
    let dir = args.next().expect("the data folder");
    let mut plan: Vec<(u64, String)> = args
        .map(|a| {
            let (at, name) = a.split_once(':').expect("<seconds>:<interface>");
            (at.parse().expect("seconds"), name.to_owned())
        })
        .collect();
    plan.sort();
    let end = plan.last().map_or(10, |(at, _)| at + 10);

    let mut core = Core::start(dir.into(), || {});
    // RUSTTORRENT_REMOTE=<port>:<password> turns remote control on.
    if let Ok(remote) = std::env::var("RUSTTORRENT_REMOTE") {
        let (port, password) = remote.split_once(':').expect("<port>:<password>");
        core.send(Command::SetRemote {
            enabled: true,
            port: port.parse().expect("port"),
            lan: false,
            password: Some(password.to_owned()),
        });
    }
    let start = Instant::now();
    loop {
        let at = start.elapsed().as_secs();
        if at >= end {
            break;
        }
        while plan.first().is_some_and(|(when, _)| *when <= at) {
            let (_, name) = plan.remove(0);
            println!("{at:>3}s  choosing {name}");
            core.send(Command::SetInterface(Some(name).filter(|n| n != "any")));
        }
        let snap = core.snapshot();
        let torrents: Vec<String> = snap
            .torrents
            .iter()
            .map(|t| format!("{} {:.1}%", t.state.label(), t.progress() * 100.0))
            .collect();
        println!(
            "{at:>3}s  blocked: {:?}  down: {} KiB/s  [{}]",
            snap.blocked.as_deref().unwrap_or("no"),
            snap.down_bps / 1024,
            torrents.join(", ")
        );
        std::thread::sleep(Duration::from_secs(1));
    }
    core.shutdown();
}
