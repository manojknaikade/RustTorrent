# RustTorrent

A torrent client for the Mac, written in Rust. It can be compiled for Windows and Linux too.

It exists because Transmission hides a torrent's details in a small popup and qBittorrent is awkward to install on Apple Silicon. RustTorrent keeps everything about a torrent in the main window, and aims to stay small and fast: one binary, no web engine.

Status: **0.13, early**. It installs with Homebrew and has been tested on real downloads. What it has and lacks next to qBittorrent is listed in [the comparison](docs/designs/qbittorrent-gap.md).

## Install

On an Apple Silicon Mac, with [Homebrew](https://brew.sh):

```bash
brew install --cask manojknaikade/tap/rusttorrent
```

This puts RustTorrent in `/Applications`. Later versions arrive with `brew upgrade`.

To have magnet links in a browser open in RustTorrent, open Settings in the app: the last line says which app macOS uses for magnet links, with a button to change it to RustTorrent.

## What works in 0.13

Adding

- Add torrents by magnet link, `.torrent` file, web address of a `.torrent` file, or bare info-hash.
- Add by clicking a magnet link in a browser, opening a `.torrent` file from Finder, dropping a file on the window, or pasting into the add box.
- Add dialog: choose the folder, category, files and "start paused" before the download begins. It appears when "ask where to save" is on in Settings.
- Folder buttons open a menu of the last 8 folders used, with "Other folder…" to pick a new one.
- Watch folders: `.torrent` files put in a chosen folder are added by themselves. There can be several folders, and each can give its torrents a category. A file that has been added is renamed to end in `.added`.
- Import from Transmission or qBittorrent (Settings): their torrents are added with the folder each one saved to, its category and whether it was paused. The data stays where it is and is checked before the download goes on.
- RSS and Atom feeds, read every 15 minutes. A feed's rule can use word lists or a regular expression, an episode filter ("S02E05 and later"), skip repeats of an episode, and take only items newer than the ones already listed. A preview shows a feed's items with a Download button for each. A feed can give its torrents a category.

Managing

- Pause, resume, force start (ignore the queue) and remove (optionally deleting the downloaded files).
- Choose which files of a torrent to download, and give each file a priority: high, normal or low.
- Recheck a torrent's data, reannounce it to its trackers, or move its data to another folder (right-click menu). A move takes the torrent out of the engine, moves the files, then adds it back at the new folder, where its data is checked again. A move cut short by quitting finishes on the next start.
- A torrent with more than one file gets a folder named after it inside the download folder. A torrent with one file is saved straight into the download folder. Torrents added by older versions keep the folder they had; moving one's data puts it in a folder of its own name.
- A folder for unfinished downloads (Settings): torrents download there and are moved to the download folder when they finish. The finish notice and the finish program wait until the move is done. A torrent whose data is already in the download folder starts there.
- Categories and tags, with filters in the sidebar. Make a category with + in the sidebar or by typing a name in the add dialog; right-click one in the sidebar to set its folder or delete it. A category can have its own download folder: a torrent put in that category is saved there, and its data is moved there if it was already downloading.
- Copy a torrent's magnet link, info-hash or name. Save its `.torrent` file.
- Make a `.torrent` from a file or folder: trackers in tiers, piece size, comment, private flag, and start seeding it straight away.
- Downloads go in order: high-priority files first, then normal, then low, by name within each, each file from its start to its end (the first and last piece of a file come first, so a player can read its index). This is how the engine works, so it cannot be switched off.
- Play a file of a torrent, or copy a link to it, from the Files tab, even before it has finished: the parts being played are downloaded first. The link is served by the app on this computer only. Play opens VLC if it is installed, otherwise the default app for web links.
- Disk guard: downloads are paused when their disk has less than a chosen amount free (1 GB by default; Settings), with a notice saying so.
- Torrents carry on after the app is restarted. The app warns before quitting while downloads are running.

Limits

- Global download and upload speed limits, limits for one torrent (in its details), and "slow mode": a second set of limits switched from the status bar or by its own schedule.
- Queue: how many torrents download at once, seed at once, and are active at once; slow torrents can be left out of the count. Reorder from the right-click menu.
- Stop seeding at a ratio (for all torrents or for one), after a seeding time, or after a time without upload.
- Peers per torrent.
- Schedule: allow transfers only between two times of day, on chosen days.

Network

- Listening port, and switches for UPnP, DHT and local peer discovery. What the DHT learns is kept in the app's own data folder.
- uTP: also connect to peers over UDP. Off by default.
- SOCKS5 proxy.
- IP block list, and "ban this peer" in the peers view.
- Network interface binding, for use with a VPN: choose one interface in Settings and all torrent traffic and feed reading goes through it. When that interface is not connected, the torrent engine is stopped, so nothing is sent or received until it is back; the window shows a red banner saying so.

The window

- Details in the main window: overview with a piece map, files, peers and trackers for the selected torrent.
- Sortable table with a column chooser, search, and filters by status, category and tag.
- Speed graph in the toolbar, a Statistics window with all-time totals, and a Log window.
- Dark and light themes.
- When a download finishes: a notification, and optionally a program is run (`%N` is the torrent's name, `%F` its folder). When all downloads finish, the app can quit, or put the computer to sleep or shut it down after a 30-second countdown that can be cancelled.
- Keeps the Mac awake while transferring, if switched on.

Remote control

- A small web page, served by the app itself, for a browser or phone. It can also switch slow mode and set the seeding and queue limits. It is off until you turn it on in Settings and set a password. See [Remote control](#remote-control).
- A part of qBittorrent's Web API on the same port and password, so Sonarr, Radarr and phone apps made for qBittorrent can use RustTorrent. Point them at the same address; any user name works.

## Size and speed

Measured on an Apple Silicon Mac. Size with 0.13.4, the rest with 0.13.3:

| | Measured | Target |
| --- | --- | --- |
| App on disk | 14 MB (6.8 MB zipped) | under 15 MB |
| Memory with one finished 735 MB torrent seeding | 49–53 MB footprint | under 100 MB |
| Test download (735 MB Debian image) | finished in under 25 seconds, checksum matched | |

Memory is the footprint that Activity Monitor shows, not the resident size. Older versions of this page gave the resident size (about 104 MB with 0.12), so the two numbers are not directly comparable. Most of the app's size is the drawing library.

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
- The page can do a fixed, short list of things: add a magnet link, web address or uploaded `.torrent` file (up to 4 MiB), pause, resume, remove, choose which of a torrent's files to download, set its category, set speed limits, slow mode, and the seeding and queue limits. It cannot read or choose folders on the computer, and it cannot change the app's other settings.
- The qBittorrent API has the same limits as the page and goes through the same sign-in, host and origin checks. It cannot change folders or the app's preferences.
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
- The remote page cannot set the slow-mode schedule or the transfer schedule. It was tested in one desktop browser, not on a real phone; uploading a valid `.torrent` file, the Files dialog and HTTPS were checked by automated tests only.
- Switching in-order download off is missing: the engine cannot do it without a large change.
- Recheck, reannounce, per-torrent limits and file priorities come from a patched copy of the engine (librqbit), pinned in `Cargo.toml`, until the changes are accepted upstream.
- With a folder for unfinished downloads, if the app quits between a finish and the move, the finish notice and program for that torrent are skipped; the move itself still happens.
- Torrents are added to the engine one at a time. Many added at once (a dropped batch, a watch folder, an import) queue for a moment. The engine gave two adds at the same moment the same number, and called the second one a duplicate.
- While a torrent's data is being moved it cannot be removed, and its transfers stop until the move ends.
- A torrent stopped by a seeding time limit stays stopped when resumed, until the limit is raised.
- The proxy password is kept as plain text in the settings file. The block list is read only when the engine starts. Changing a network setting restarts the engine, which interrupts transfers for a moment.
- The finish notification was never seen: macOS refused notification permission to the locally signed app on the test Mac. Keeping the Mac awake was not confirmed by measurement. The proxy and the block list were checked by automated tests only.
- There is no menu-bar icon; closing the window quits the app.
- The qBittorrent API was tested with automated tests over a real connection, not with real Sonarr, Radarr or phone apps. A folder given on add (`savepath`) is ignored, and moving a torrent (`setLocation`) and changing preferences are refused. Tags given on add are ignored. Some numbers it reports are estimates: seeds and leechers are not told apart, and finish times, tracker status and piece counts are not known. A program in Docker that reaches the app under a host name other than `localhost`, a `.local` name or an IP address is refused by the host check; use the IP address.
- Playing was tested with a torrent made and seeded on the same computer, not while downloading from other peers, and not with VLC itself. At most 3 files play at once. A link copied while a torrent runs keeps working after it is paused, but then stalls on any part not yet downloaded.
- The disk guard does not resume downloads when room is made again; resume them by hand. On Windows it does nothing, because free space is not read there yet.
- Sleep and shut down were checked by automated tests only, not by letting a Mac sleep or shut down.
- Import was tested against hand-made files only, not files written by Transmission or qBittorrent. Transmission's folder for unfinished downloads is not read. qBittorrent entries that never got their `.torrent` data (magnet links still looking for it) are skipped.
- The "Open them in RustTorrent" button in Settings was not tested: on the test Mac, RustTorrent was already the app for magnet links.
- Built and tested on Apple Silicon only. The Homebrew install is for Apple Silicon only.

## License

MIT. See [LICENSE](LICENSE).
