//! Probe of librqbit 9.1.0: checks at run time what the source code suggests.
//! Usage: engine-probe <torrent-url> <work-dir> [network-interface]
use std::{io::Read, num::NonZeroU32, path::Path, time::Duration};

use anyhow::Context;
use librqbit::{
    AddTorrent, AddTorrentOptions, Session, SessionOptions, SessionPersistenceConfig,
};

fn opts(work: &Path, iface: Option<&str>) -> SessionOptions {
    SessionOptions {
        fastresume: true,
        persistence: Some(SessionPersistenceConfig::Json {
            folder: Some(work.join("session")),
        }),
        bind_device_name: iface.map(str::to_owned),
        ..Default::default()
    }
}

/// Counts 1 MiB blocks holding data, and how many of them form an unbroken run from the start.
fn block_map(path: &Path) -> anyhow::Result<(usize, usize, usize)> {
    let mut f = std::fs::File::open(path)?;
    let mut buf = vec![0u8; 1 << 20];
    let (mut total, mut filled, mut prefix, mut in_prefix) = (0, 0, 0, true);
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        total += 1;
        if buf[..n].iter().any(|b| *b != 0) {
            filled += 1;
            if in_prefix {
                prefix += 1;
            }
        } else {
            in_prefix = false;
        }
    }
    Ok((total, filled, prefix))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let url = args.get(1).context("torrent url")?.clone();
    let work = std::path::PathBuf::from(args.get(2).context("work dir")?);
    let iface = args.get(3).map(String::as_str);
    let out = work.join("downloads");

    // ---- Run 1: add, download, limit, pause ----
    let session = Session::new_with_opts(out.clone(), opts(&work, iface)).await?;
    println!("[bind] session started with interface {iface:?}");
    let h = session
        .add_torrent(
            AddTorrent::from_url(url),
            Some(AddTorrentOptions { overwrite: true, ..Default::default() }),
        )
        .await?
        .into_handle()
        .context("no handle")?;
    h.wait_until_initialized().await?;
    println!("[add] name={:?} total={}", h.name(), h.stats().total_bytes);

    for i in 0..6 {
        tokio::time::sleep(Duration::from_secs(5)).await;
        let s = h.stats();
        let peers = s.live.as_ref().map(|l| l.snapshot.peer_stats.live).unwrap_or(0);
        let speed = s.live.as_ref().map(|l| l.download_speed.to_string()).unwrap_or_default();
        println!("[download] t={}s bytes={} peers={} speed={}", (i + 1) * 5, s.progress_bytes, peers, speed);
        if i == 2 {
            session.ratelimits.set_download_bps(NonZeroU32::new(200 * 1024));
            println!("[limit] download limit set to 200 KiB/s while running");
        }
    }
    let before_pause = h.stats();
    session.pause(&h).await?;
    println!("[pause] is_paused={} uploaded_bytes_in_stats={}", h.is_paused(), h.stats().uploaded_bytes);
    println!("[pause] uploaded before pause={} (live counter)", before_pause.uploaded_bytes);

    let name = h.name().unwrap_or_default();
    let file = out.join(&name);
    if file.is_file() {
        let (total, filled, prefix) = block_map(&file)?;
        println!("[order] {total} MiB blocks, {filled} hold data, {prefix} of those are an unbroken run from the start");
    } else {
        println!("[order] {file:?} is not a single file; skipped");
    }
    let bytes_run1 = h.stats().progress_bytes;
    session.stop().await;
    drop(h);
    drop(session);
    tokio::time::sleep(Duration::from_secs(2)).await;

    // ---- Run 2: restart, see what came back ----
    let session = Session::new_with_opts(out, opts(&work, iface)).await?;
    tokio::time::sleep(Duration::from_secs(3)).await;
    let restored: Vec<_> = session.with_torrents(|it| it.map(|(_, h)| h.clone()).collect());
    println!("[restart] torrents restored: {}", restored.len());
    for h in restored {
        let _ = tokio::time::timeout(Duration::from_secs(60), h.wait_until_initialized()).await;
        let s = h.stats();
        println!(
            "[restart] paused={} bytes={} (was {}) uploaded_bytes={}",
            h.is_paused(), s.progress_bytes, bytes_run1, s.uploaded_bytes
        );
        session.unpause(&h).await?;
        tokio::time::sleep(Duration::from_secs(5)).await;
        println!("[resume] state after unpause: {}", h.stats().state);
    }
    session.stop().await;
    Ok(())
}
