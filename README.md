# RustTorrent

A torrent client for the Mac, written in Rust. It can be compiled for Windows and Linux too.

It exists because Transmission hides a torrent's details in a small popup and qBittorrent is awkward to install on Apple Silicon. RustTorrent keeps everything about a torrent in the main window, and aims to stay small and fast: one binary, no web engine.

Status: **0.1, early**. The core works and has been tested on a real download. Many planned features are not built yet; see the list below.

## What works in 0.1

- Add torrents by magnet link, `.torrent` file, web address of a `.torrent` file, or bare info-hash.
- Add by clicking a magnet link in a browser, opening a `.torrent` file from Finder, dropping a file on the window, or pasting into the add box.
- Pause, resume and remove (optionally deleting the downloaded files).
- Choose which files of a torrent to download.
- Global download and upload speed limits.
- Torrents carry on after the app is restarted.
- Details in the main window: overview, files, peers and trackers for the selected torrent.
- Sortable table, search, and filters by status.

## Size and speed

Measured on an Apple Silicon Mac with version 0.1:

| | Measured | Target |
| --- | --- | --- |
| App on disk | 16 MB (7.7 MB zipped) | under 15 MB |
| Memory when idle, one torrent seeding | about 70 MB | under 100 MB |
| Processor when idle | about 0.1% | |
| Test download (792 MB Debian image) | finished in under a minute at about 38 MB/s, checksum matched | |

The app is 1 MB over its size target. Most of the size is the drawing library.

## Planned

In this order, as described in [the design document](docs/designs/rusttorrent.md):

1. Download queue, seeding ratio limits, schedule, reliable in-order download.
2. Categories and tags, watch folder, RSS auto-download.
3. Binding to one network interface (for VPN use), blocking all traffic when it is down.
4. Remote control from a browser or phone.
5. Homebrew tap, app icon.

There is no plugin support and none is planned.

## Build

You need [Rust](https://rustup.rs).

On a Mac, build the app bundle:

```bash
./scripts/bundle-macos.sh
```

This writes `dist/RustTorrent.app`. Move it to `/Applications` and open it once so macOS learns that it handles magnet links.

To make it the default for magnet links, open a magnet link in your browser and pick RustTorrent when asked. If another torrent client is already the default, change it in that client's settings or with a tool such as `duti`.

On Windows or Linux, or to run without a bundle:

```bash
cargo run --release -p rusttorrent
```

Linux needs the usual window-system packages, for example `libgtk-3-dev libxkbcommon-dev libwayland-dev` on Ubuntu. Clicking magnet links is only wired up on the Mac so far; on other systems pass the link as an argument or paste it into the add box.

## Where things are kept

| What | Where (Mac) |
| --- | --- |
| Torrent list and settings | `~/Library/Application Support/RustTorrent/state.json` |
| Log | `~/Library/Application Support/RustTorrent/rusttorrent.log` |
| Downloads | `~/Downloads` unless changed in Settings |

Set `RUSTTORRENT_DATA_DIR` to keep the first two somewhere else.

## Layout of the code

- `crates/core` holds every rule of the app and talks to the torrent engine ([librqbit](https://github.com/ikatson/rqbit)). It has no window code.
- `crates/app` is the desktop window, drawn with [egui](https://github.com/emilk/egui).
- `spikes/` holds the small experiments that were run before the design was settled.

```bash
cargo test --workspace
```

## Known gaps

- No app icon yet.
- The app is signed only locally ("ad-hoc"), so a copy downloaded from the internet will be blocked by macOS until it is allowed in System Settings. Building it yourself avoids this.
- Built and tested on Apple Silicon only.

## License

MIT. See [LICENSE](LICENSE).
