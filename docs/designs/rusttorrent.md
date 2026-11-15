# RustTorrent

* **Author:** manojknaikade
* **Date:** 2026-11-15

## Overview

RustTorrent is a torrent client for the Mac, written in Rust. It is for people who find Transmission's and qBittorrent's interfaces poor. Transmission's "download in order" does not work for the author, and its torrent details open in a popup that shows little. qBittorrent does not install through Homebrew on the author's Apple Silicon Mac.

The app uses the `librqbit` library to speak the BitTorrent protocol and `egui` to draw its window. It is MIT licensed, installed from the author's own Homebrew tap, and can be compiled on Windows and Linux by anyone who wants to.

Version one must do the following:

- Open when a magnet link is clicked in a browser (compulsory).
- Download a torrent's data in order, so a video can be watched while it downloads.
- Show a torrent's full details and progress in the main window, not in a popup.
- Open `.torrent` files, pause, resume and remove torrents, choose which files to download, limit speeds, and carry on after a restart.
- Queue downloads, stop seeding at a set ratio, group torrents by category or tag, download automatically from RSS feeds, follow a schedule, watch a folder for new `.torrent` files, send traffic only through a chosen network interface (for VPN use), and be controlled from a browser.

There is no plugin support. Where speed and size conflict, speed wins. Size is cut everywhere else. Working targets: app under 15 MB, under 100 MB of memory when idle.

## Analysis

Seven independent reviewers read the implementation plan without seeing the conversation that produced it. None of them could run code or check the web, so every statement about what librqbit, egui or Homebrew can do is recollection and must be tested. Their findings, strongest first:

**1. The compulsory magnet-link feature has no working fallback.** The plan says that if the app cannot receive a magnet click directly, it will fall back to reading the link from the command line. macOS never passes a clicked link that way. It sends an "Apple Event" to the running app. So the fallback does nothing. The plan also schedules this feature fourth, after the whole window is built on a toolkit that may not be able to receive that event, and never tests it. Other gaps: the first click can be lost if the app is closed when it arrives; declaring support for magnet links does not make the app the default if Transmission already holds that role; opening a `.torrent` file from Finder uses the same mechanism.

**2. "Better interface" is not defined, and the plan copies qBittorrent's window.** The plan's stated goal is performance and size. It does not mention interface quality, which is the reason the project exists. Its layout (toolbar, status sidebar, table, detail tabs at the bottom) is the same as qBittorrent's. Without a written list of what must be different, nobody can judge whether egui is the right choice or whether the result is an improvement.

**3. Nobody has checked what the engine library can do.** The plan leaves this "to be confirmed when implementing". Reviewers expect librqbit to handle pause and resume, file selection, restart recovery, global speed limits and roughly in-order downloading. They expect it to lack queueing and ratio limits. They do not know whether it records upload totals across restarts (without that, ratio limits never trigger), whether it has a true in-order setting, whether it can bind to one network interface on macOS, or whether it reports tracker status. The library has one main maintainer and changes its programming interface between major versions.

**4. The plan's two-part structure does not fit the feature list.** The plan has an engine part and a window part. Queueing, ratio limits, scheduling, RSS, the watch folder and remote control are background rules that belong in neither. If they run inside the window's drawing code they stop when the window is hidden. Five different things can pause a torrent (the user, the queue, the schedule, a ratio limit, a lost VPN); with a single "paused" flag they will override each other. Nothing in the plan stores categories, queue order, ratio limits, RSS rules or schedules.

**5. VPN binding must block all traffic when the VPN is down, or not ship.** Binding has to cover peer connections, the peer-discovery network, tracker requests, web downloads and RSS fetches. Tracker requests are the usual leak. macOS VPN interface names change between connections. Pausing when the VPN disappears is not enough, because data can leave in the gap before the pause.

**6. Remote control is a second interface and the main security risk.** Any web page open in the browser can send requests to a server running on the same machine. Transmission had this flaw. Remote control must be off by default, limited to the local machine unless changed, require a password, and check where requests come from.

**7. Distribution is under-planned.** Apple Silicon requires at least a local ("ad-hoc") signature. Unsigned builds may trigger the firewall prompt on every update. Homebrew is reported to be restricting installs that remove the quarantine flag; whether that affects personal taps is unchecked. The plan has no release builds, checksums, tap repository or update path.

**8. Reliability gaps.** The plan's setting `panic = "abort"` means one fault in any background task closes the whole app. The window toolkit quits when the window closes, and macOS slows down hidden apps, both of which stop downloads. There is no log file, no per-torrent error display, and no handling for a full disk, an unplugged drive, a port already in use, or a second copy of the app.

**9. egui's real costs.** No native menu bar without an extra library; torrent names in non-Latin scripts show as boxes unless fonts are loaded; table sorting and multi-select are written by hand; the chosen drawing method uses OpenGL, which Apple has deprecated. One reviewer noted the author's stated complaints are about layout, which egui can deliver.

**Trade-offs the design accepts, and ways around them:**

- *Small size versus a second (browser) interface.* One reviewer proposed building a single web interface shown in a system web view, which also serves as the remote. This likely breaks the 100 MB memory target while the window is open.
- *Compulsory magnet clicks versus a toolkit that may not receive them.* A very small helper written in Swift can register as the magnet handler and pass the link to the main app, keeping egui.
- *Many features that pause torrents versus predictable behaviour.* Store what the user asked for separately from the reasons a torrent is held, and let one function decide the final state. That function can be tested with a fake engine and a fake clock.
- *Many ways to add a torrent.* Magnet clicks, files, drag-and-drop, the watch folder, RSS and the remote all go through one intake path.

**Ideal result:** a clicked magnet link opens the app and starts downloading in order with no further steps; every rule (queue, schedule, ratio, VPN) runs whether or not the window is open; no traffic leaves outside the chosen interface; and the app stays within its size and memory targets.

## Goals vs. Non-Goals

* **In-Scope Goals:**
  * Magnet-link clicks and `.torrent` files open the app and add the torrent.
  * In-order download that lets a partly downloaded video play from the start, set per torrent, kept after restart.
  * Torrent details and progress shown in the main window.
  * Queueing, ratio limits, categories or tags, RSS auto-download, scheduling, watch folder, network-interface binding, browser remote control.
  * Install from a personal Homebrew tap on Apple Silicon.
  * Compiles on Windows and Linux (checked by automated builds only).
* **Out-of-Scope Non-Goals:**
  * Plugin support.
  * A native Mac look.
  * Listing in Homebrew's official catalogue.
  * Notarized distribution (no Apple Developer account assumed).
  * Magnet-link registration, installers or testing on Windows and Linux.
  * Writing BitTorrent protocol code from scratch.

## Alternatives Weighed

* **Tauri (web interface in the system web view).** Rejected by author: uses roughly 150–250 MB of memory because of WebKit helper processes; the author chose lower memory. Reviewers raised it again as a way to get the remote interface for free.
* **SwiftUI interface with a Rust core.** Rejected by author: the interface would run on the Mac only, and the author accepts a non-native look if performance and appearance are good.
* **Terminal interface.** Rejected by author: no window, dock icon or magnet-link handling.
* **Background service with a browser interface only.** Rejected by author in favour of a desktop app.
* **Writing the BitTorrent engine from scratch.** Rejected by author: far more work and less reliable for a long time; librqbit chosen.
* **Plugin support.** Rejected by author: conflicts with small size and adds security risk.
* **Homebrew official catalogue.** Rejected by author for now: requires notarization and project popularity; personal tap chosen.
* **Reusing librqbit's built-in web page for remote control.** Open. Two reviewers favour it for less work; two say it bypasses the app's own queue and categories.

## Key Decisions & Staging for Next Steps

Decided:

1. egui interface, librqbit engine, MIT license, repository name `RustTorrent`, public on GitHub.
2. Personal Homebrew tap for the first release.
3. No plugins.
4. Speed wins over size where they conflict; targets are under 15 MB installed and under 100 MB idle memory.
5. The full feature list above is the author's definition of version one.

Reviewer recommendations, accepted by the author on 2026-11-15:

1. Run three short tests before building anything else:
   * a bundled empty window that shows a magnet link clicked in Safari, Chrome and Firefox, with the app both closed and running;
   * a command-line probe of a pinned librqbit version, producing a table of each feature as built in, buildable on top, or impossible;
   * an egui mock-up with fake data, non-Latin names and thousands of rows for the author to approve.
2. Restructure into three layers: an engine adapter, a core library holding all rules and stored data with no dependency on egui, and front ends (window, remote control, system events).
3. Deliver version one as a series of tagged 0.x releases ordered by risk: tests, core and storage, main window, queue/ratio/schedule, categories/watch folder/RSS, VPN binding, remote control last.
4. Drop the plan's extras that are not on the version-one list (create `.torrent`, notifications, sleep prevention).
5. Replace `panic = "abort"` with the default, add a log file and per-torrent error display.
6. Ship for Apple Silicon only; a build for both chip types roughly doubles the size.

Test results, 2026-11-15 (code in `spikes/`):

**Test 1, magnet links: passed, with one step left for the author.** A bundled egui test app received magnet links and `.torrent` files from macOS, both when closed and when already running (four cases, all passed). Two additions to the window toolkit were needed, about 60 lines in total:

- File opens: the method `application:openURLs:` is added to the toolkit's app delegate just before launch finishes.
- Magnet links: macOS did not send links to that method when it was added late, so the app registers its own handler for the "get URL" Apple Event. The first attempt without this received files but no links.

No Swift helper and no toolkit change is needed. Not yet tested: clicks from Safari, Chrome and Firefox. The test sent links with the `open` command, which uses the same delivery path, but a browser click goes to the default magnet app, which on this Mac is Motrix. Changing the default is a system setting the author must make.

**Test 2, engine probe: passed.** Tested with librqbit 9.1.0 on a 735 MB Debian image (about 55 MiB/s, 53 to 66 peers).

| Feature | Result |
| --- | --- |
| In-order download | Built in and always on. Of 702 one-megabyte blocks, 701 held data and all 701 formed an unbroken run from the start. Order is: files by name, then each file's first piece, last piece, then start to end. There is no setting to turn it off or to prefer rare pieces. |
| Pause, resume, remove | Built in. Tested. |
| Choose files | Built in (at add time and later). |
| Speed limits | Built in. A global limit set while running took effect within five seconds. Per-torrent limits can be set when adding. |
| Resume after restart | Built in. The torrent came back paused with the same byte count. |
| Watch folder | Built in. |
| Bind to one network interface | Built in. Covers peer connections, DHT, UDP and HTTP trackers, local discovery and port forwarding. With an interface that does not exist the engine refuses to start, so nothing leaks at start-up. |
| Upload totals across restarts | Not kept. The counter is zero after a restart. The app must store its own totals for ratio limits. |
| Queue, ratio limits, categories, schedule, RSS | Not in the library. Buildable on top, in the core layer. |
| Per-tracker status | Not available without changing the library. The Trackers tab can list trackers but not their state. |
| Web seeds | Not supported by the library. |

Not yet verified for interface binding: whether name lookups (DNS) go through the bound interface, what happens when the interface disappears while running, and the proxy path (a SOCKS proxy bypasses the binding for web requests). The app's own RSS fetches must use a bound connection.

**Test 3, interface mock-up: built, awaiting the author's approval.** `spikes/ui-mock` shows 5,000 fake torrents with names in Japanese, Chinese, Hindi, Russian, Arabic and Korean. Picture: `spikes/ui-mock/mockup.png`.

- Non-Latin names display correctly. egui 0.37 loads system fonts on demand, so no fonts are bundled. Chinese text mixes two fonts in places.
- Size: 9.0 MB for the mock-up, 13 MB for the magnet test app (not size-optimised), 13.7 MB for the engine probe (not size-optimised). The combined app's size is not yet known and the 15 MB target is at risk.
- Memory: 42 MB footprint with the window open and idle, 0% CPU when idle. This excludes the engine.
- Not measured: scrolling smoothness. Only visible rows are drawn.
- The drawing method is Metal (through wgpu), not the deprecated OpenGL.

Open questions for the author:

1. Does the mock-up layout meet the bar, and what should change?
2. Per-tracker status: accept a Trackers tab without status in version one, or patch the library?
3. In-order download cannot be turned off in the engine. Accept "always in order" for version one, or patch the library to add a rarest-first choice?
4. Is idle memory measured with the window open or closed?
5. Is the 99-dollar Apple Developer account worth paying before sharing the app with others?
