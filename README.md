# RustTorrent

A torrent client for the Mac, written in Rust. It can be compiled for Windows and Linux too.

It exists because Transmission hides a torrent's details in a small popup and qBittorrent is awkward to install on Apple Silicon. RustTorrent keeps everything about a torrent in the main window, and aims to stay small and fast: one binary, no web engine.

Status: **0.5, early**. Every feature planned for the first version is built and has been tested on real downloads. Packaging (Homebrew, an app icon) is still to come.

## What works in 0.5

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
- Network interface binding, for use with a VPN: choose one interface in Settings and all torrent traffic and feed reading goes through it. When that interface is not connected, the torrent engine is stopped, so nothing is sent or received until it is back; the window shows a red banner saying so.
- Remote control from a browser or phone: a small web page, served by the app itself, that shows the torrents and can add links, pause, resume, remove and set speed limits. It is off until you turn it on in Settings and set a password. See [Remote control](#remote-control).
- Sortable table, search, and filters by status. The details show what a waiting torrent is waiting for.

## Size and speed

Measured on an Apple Silicon Mac (size with 0.5, memory with 0.3, download test with 0.1):

| | Measured | Target |
| --- | --- | --- |
| App on disk | 16 MB (7.7 MB zipped) | under 15 MB |
| Memory with two finished torrents seeding | about 61 MB | under 100 MB |
| Processor when idle | about 0.1% | |
| Test download (792 MB Debian image) | finished in under a minute at about 38 MB/s, checksum matched | |

The app is 1 MB over its size target. Most of the size is the drawing library.

## Planned

In this order, as described in [the design document](docs/designs/rusttorrent.md):

1. Homebrew tap, app icon.

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

## Remote control

In Settings, tick "Remote control from a browser", type a password of at least 8 characters and save. Then open `http://localhost:8787` in a browser on the same computer. To use it from a phone, also tick "Allow other devices on my network"; Settings then shows the address to open on the phone.

What protects it:

- It is off by default, and without "Allow other devices" it answers only on the computer itself.
- Nothing is shown or done without signing in. The password is kept only as a salted, slow hash (PBKDF2, 600,000 rounds). Sign-in is limited to one try a second, with a 30-second lock after every fifth wrong password.
- Requests are checked for where they come from, so a web page open in another tab cannot send commands, and the page cannot be reached under a made-up host name.
- The page can do a fixed, short list of things: add a magnet link or web address, pause, resume, remove, set speed limits. It cannot read or choose files or folders on the computer, and it cannot change the app's other settings.
- Changing the password, the port or the network choice signs everyone out.

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
- Interface binding was tested with the Mac's normal interface and with an interface name that does not exist, not with a real VPN.
- With an interface chosen, name lookups (DNS) still go through the system's resolver, which may not use that interface. Addresses of trackers and feeds can be looked up outside the VPN; the transfers themselves cannot.
- If a VPN comes back under a different interface name (for example `utun5` instead of `utun4`), the app stays blocked until the new name is chosen in Settings.
- While traffic is blocked, torrents cannot be removed and their file choice cannot be changed.
- Interface binding works on the Mac and Linux. On Windows the interface list is empty.
- Remote control is not encrypted. On "Allow other devices", the password and everything else travel in the clear, so anybody on the same network who can listen in can read them. Use it only on a network you trust, and do not open the port to the internet.
- Remote control does not follow the chosen network interface: with a VPN interface chosen it still answers on the normal network.
- The remote page cannot upload a `.torrent` file, choose files inside a torrent, or set categories. It was tested in one desktop browser at desktop and phone width, not on a real phone.
- Built and tested on Apple Silicon only.

## License

MIT. See [LICENSE](LICENSE).
