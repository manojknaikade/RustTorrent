# RustTorrent

A torrent client for the Mac, written in Rust. It can be compiled for Windows and Linux too.

It exists because Transmission hides a torrent's details in a small popup and qBittorrent is awkward to install on Apple Silicon. RustTorrent keeps everything about a torrent in the main window, and aims to stay small and fast: one binary, no web engine.

Status: **0.12, early**. It installs with Homebrew and has been tested on real downloads. What it has and lacks next to qBittorrent is listed in [the comparison](docs/designs/qbittorrent-gap.md).

## Install

On an Apple Silicon Mac, with [Homebrew](https://brew.sh):

```bash
brew install --cask manojknaikade/tap/rusttorrent
```

This puts RustTorrent in `/Applications`. Later versions arrive with `brew upgrade`.

To have magnet links in a browser open in RustTorrent, open Settings in the app: the last line says which app macOS uses for magnet links, with a button to change it to RustTorrent.

## What works in 0.12

Adding

- Add torrents by magnet link, `.torrent` file, web address of a `.torrent` file, or bare info-hash.
- Add by clicking a magnet link in a browser, opening a `.torrent` file from Finder, dropping a file on the window, or pasting into the add box.
- Add dialog: choose the folder, category, files and "start paused" before the download begins. It appears when "ask where to save" is on in Settings.
- Watch folder: `.torrent` files put in a chosen folder are added by themselves. A file that has been added is renamed to end in `.added`.
- RSS and Atom feeds, read every 15 minutes. A feed's rule can use word lists or a regular expression, an episode filter ("S02E05 and later"), skip repeats of an episode, and take only items newer than the ones already listed. A preview shows a feed's items with a Download button for each. A feed can give its torrents a category.

Managing

- Pause, resume, force start (ignore the queue) and remove (optionally deleting the downloaded files).
- Choose which files of a torrent to download.
- Categories and tags, with filters in the sidebar. A category can have its own download folder.
- Copy a torrent's magnet link, info-hash or name. Save its `.torrent` file.
- Make a `.torrent` from a file or folder: trackers in tiers, piece size, comment, private flag, and start seeding it straight away.
- Downloads go in order: files one after another by name, each from its start to its end (the first and last piece of a file come first, so a player can read its index). This is how the engine always works, so it cannot be switched off.
- Torrents carry on after the app is restarted. The app warns before quitting while downloads are running.

Limits

- Global download and upload speed limits, and "slow mode": a second set of limits switched from the status bar or by its own schedule.
- Queue: how many torrents download at once, seed at once, and are active at once; slow torrents can be left out of the count. Reorder from the right-click menu.
- Stop seeding at a ratio (for all torrents or for one), after a seeding time, or after a time without upload.
- Peers per torrent.
- Schedule: allow transfers only between two times of day, on chosen days.

Network

- Listening port, and switches for UPnP, DHT and local peer discovery.
- SOCKS5 proxy.
- IP block list, and "ban this peer" in the peers view.
- Network interface binding, for use with a VPN: choose one interface in Settings and all torrent traffic and feed reading goes through it. When that interface is not connected, the torrent engine is stopped, so nothing is sent or received until it is back; the window shows a red banner saying so.

The window

- Details in the main window: overview with a piece map, files, peers and trackers for the selected torrent.
- Sortable table with a column chooser, search, and filters by status, category, tag and tracker.
- Speed graph in the toolbar, a Statistics window with all-time totals, and a Log window.
- Dark and light themes.
- When a download finishes: a notification, and optionally a program is run (`%N` is the torrent's name, `%F` its folder). When all downloads finish, the app can quit.
- Keeps the Mac awake while transferring, if switched on.

Remote control

- A small web page, served by the app itself, for a browser or phone. It is off until you turn it on in Settings and set a password. See [Remote control](#remote-control).

## Size and speed

Measured on an Apple Silicon Mac with 0.12:

| | Measured | Target |
| --- | --- | --- |
| App on disk | 17 MB (8.5 MB zipped) | under 15 MB |
| Memory with one finished 735 MB torrent seeding | about 104 MB | under 100 MB |
| Processor at that time | about 0.5% | |
| Test download (735 MB Debian image) | finished in under 25 seconds, checksum matched | |

The app is 2 MB over its size target and a little over its memory target. Most of the size is the drawing library.

## Planned

Everything in [the design document](docs/designs/rusttorrent.md) is built, and so is most of the plan in [the comparison with qBittorrent](docs/designs/qbittorrent-gap.md), which also lists what was left out and what the torrent engine cannot do yet.

There is no plugin support and none is planned.

## Build

You need [Rust](https://rustup.rs).

On a Mac, build the app bundle:

```bash
./scripts/bundle-macos.sh
```

This writes `dist/RustTorrent.app`. Move it to `/Applications` and open it once so macOS learns that it handles magnet links, then make it the default in the app's Settings.

The icon is drawn in `assets/icon.svg`. `assets/RustTorrent.icns` is made from it with `rsvg-convert` (one PNG per size into a `RustTorrent.iconset` folder) and `iconutil -c icns`.

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

In Settings, tick "Remote control from a browser", type a password of at least 8 characters and save. Then open `http://localhost:8787` in a browser on the same computer. Tick "Use HTTPS" to encrypt it: the app makes its own certificate, so the browser warns the first time, and Settings shows the certificate's fingerprint to compare. To use it from a phone, also tick "Allow other devices on my network"; Settings then shows the address to open on the phone.

What protects it:

- It is off by default, and without "Allow other devices" it answers only on the computer itself.
- Nothing is shown or done without signing in. The password is kept only as a salted, slow hash (PBKDF2, 600,000 rounds). Sign-in is limited to one try a second, with a 30-second lock after every fifth wrong password.
- Requests are checked for where they come from, so a web page open in another tab cannot send commands, and the page cannot be reached under a made-up host name.
- The page can do a fixed, short list of things: add a magnet link, web address or uploaded `.torrent` file (up to 4 MiB), pause, resume, remove, choose which of a torrent's files to download, set its category, set speed limits. It cannot read or choose folders on the computer, and it cannot change the app's other settings.
- Changing the password, the port or the network choice signs everyone out.

## Layout of the code

- `crates/core` holds every rule of the app and talks to the torrent engine ([librqbit](https://github.com/ikatson/rqbit)). It has no window code.
- `crates/app` is the desktop window, drawn with [egui](https://github.com/emilk/egui).
- `spikes/` holds the small experiments that were run before the design was settled.

```bash
cargo test --workspace
```

## Known gaps

- A torrent that has just been added may run for up to a second before the queue or schedule stops it.
- The ratio limit was checked by automated tests only, not by seeding a real torrent up to a limit.
- The app is signed only locally ("ad-hoc"), so a copy downloaded with a browser will be blocked by macOS until it is allowed in System Settings. The Homebrew install removes the download mark so the app opens; building it yourself also avoids the block.
- Interface binding was tested with the Mac's normal interface and with an interface name that does not exist, not with a real VPN.
- With an interface chosen, name lookups (DNS) still go through the system's resolver, which may not use that interface. Addresses of trackers and feeds can be looked up outside the VPN; the transfers themselves cannot.
- If a VPN comes back under a different interface name (for example `utun5` instead of `utun4`), the app stays blocked until the new name is chosen in Settings.
- While traffic is blocked, torrents cannot be removed and their file choice cannot be changed.
- Interface binding works on the Mac and Linux. On Windows the interface list is empty.
- Without "Use HTTPS", remote control is not encrypted: on "Allow other devices", the password and everything else travel in the clear. With HTTPS the certificate is self-made and names only `localhost` and the loopback addresses, so a phone's browser warns about it. Do not open the port to the internet either way.
- Remote control does not follow the chosen network interface: with a VPN interface chosen it still answers on the normal network.
- The remote page cannot see or set slow mode, the seeding limits or the queue limits. It was tested in one desktop browser, not on a real phone; uploading a valid `.torrent` file, the Files dialog and HTTPS were checked by automated tests only.
- Moving a torrent's data, force recheck, force reannounce, per-torrent speed limits, file priorities and switching in-order download off are missing because the engine cannot do them.
- A torrent stopped by a seeding time limit stays stopped when resumed, until the limit is raised.
- The proxy password is kept as plain text in the settings file. The block list is read only when the engine starts. Changing a network setting restarts the engine, which interrupts transfers for a moment.
- The finish notification was never seen: macOS refused notification permission to the locally signed app on the test Mac. Keeping the Mac awake was not confirmed by measurement. The proxy and the block list were checked by automated tests only.
- There is no menu-bar icon; closing the window quits the app.
- The "Open them in RustTorrent" button in Settings was not tested: on the test Mac, RustTorrent was already the app for magnet links.
- Built and tested on Apple Silicon only. The Homebrew install is for Apple Silicon only.

## License

MIT. See [LICENSE](LICENSE).
