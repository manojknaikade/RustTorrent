# RustTorrent

A torrent client for the Mac, written in Rust. It can be compiled for Windows and Linux too.

It exists because Transmission hides a torrent's details in a small popup and qBittorrent is awkward to install on Apple Silicon. RustTorrent keeps everything about a torrent in the main window, and aims to stay small and fast: one binary, no web engine.

Status: **0.3, early**. The core works and has been tested on real downloads. Many planned features are not built yet; see the list below.

## What works in 0.3

- Add torrents by magnet link, `.torrent` file, web address of a `.torrent` file, or bare info-hash.
- Add by clicking a magnet link in a browser, opening a `.torrent` file from Finder, dropping a file on the window, or pasting into the add box.
- Pause, resume and remove (optionally deleting the downloaded files).
- Choose which files of a torrent to download.
- Global download and upload speed limits.
- Download queue: set how many torrents download at once; the rest wait in order. Reorder from the right-click menu.
- Seeding ratio limit: stop seeding at a ratio, set for all torrents or for one.
- Schedule: allow transfers only between two times of day, on chosen days.
- Downloads go in order: files one after another by name, each from its start to its end (the first and last piece of a file come first, so a player can read its index). This is how the engine always works, so there is no switch for it.
- Torrents carry on after the app is restarted.
- Details in the main window: overview, files, peers and trackers for the selected torrent.
- Categories and tags: give a torrent one category and any number of tags from the right-click menu, then filter the list by them in the sidebar.
- Watch folder: `.torrent` files put in a chosen folder are added by themselves. A file that has been added is renamed to end in `.added`.
- RSS and Atom feeds: each feed is read every 15 minutes, and every item whose title has all the "must have" words and none of the "must not have" words is downloaded once. A feed can give its torrents a category.
- Sortable table, search, and filters by status. The details show what a waiting torrent is waiting for.

## Size and speed

Measured on an Apple Silicon Mac (size and memory with 0.3, download test with 0.1):

| | Measured | Target |
| --- | --- | --- |
| App on disk | 16 MB (7.7 MB zipped) | under 15 MB |
| Memory with two finished torrents seeding | about 61 MB | under 100 MB |
| Processor when idle | about 0.1% | |
| Test download (792 MB Debian image) | finished in under a minute at about 38 MB/s, checksum matched | |

The app is 1 MB over its size target. Most of the size is the drawing library.

## Planned

In this order, as described in [the design document](docs/designs/rusttorrent.md):

1. Binding to one network interface (for VPN use), blocking all traffic when it is down.
2. Remote control from a browser or phone.
3. Homebrew tap, app icon.

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
- A torrent that has just been added may run for up to a second before the queue or schedule stops it.
- A feed downloads every matching item it lists, including ones that were already in the feed when it was added. Use the filter words to narrow it.
- A category cannot have its own download folder yet.
- The ratio limit was checked by automated tests only, not by seeding a real torrent up to a limit.
- The app is signed only locally ("ad-hoc"), so a copy downloaded from the internet will be blocked by macOS until it is allowed in System Settings. Building it yourself avoids this.
- Built and tested on Apple Silicon only.

## License

MIT. See [LICENSE](LICENSE).
