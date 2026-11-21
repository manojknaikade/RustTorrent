# What qBittorrent has that RustTorrent lacks

* **Date:** 2026-11-15, updated 2026-11-16 for 0.12
* **Compared:** RustTorrent 0.12 against qBittorrent 5.x, from qBittorrent's documented feature list. qBittorrent was not installed for this comparison.

"Engine" below means [librqbit](https://github.com/ikatson/rqbit) 9.1, the library that does the transfers. Where a row says "engine check", the engine's source mentions the ability but nobody has yet tried it from RustTorrent. Where it says "engine lacks it", the feature needs a change inside the engine or a different engine.

## Already level

Magnet links and `.torrent` files, pause/resume/remove, choosing files, global speed limits, download queue, ratio limit, time-of-day schedule, categories and tags, watch folder, RSS auto-download, network interface binding, remote control from a browser, DHT, peer exchange, automatic port forwarding (UPnP), resume after restart, search and status filters, peers and trackers views.

Better than qBittorrent today: in-order download is always on and works; all details are in the main window; the app is 17 MB and installs with one Homebrew command on Apple Silicon.

## Row by row

The tables were first written against 0.6. A row now says which release closed it.

### Adding and managing torrents

| qBittorrent feature | RustTorrent | Note |
| --- | --- | --- |
| Add dialog: choose folder, category, files and "start paused" before the download begins | Has it (0.7) |  |
| Move a torrent's data to another folder | Has it | Takes the torrent out of the engine, moves the files, adds it back |
| Folder per category | Has it (0.7) |  |
| Separate folder for unfinished downloads | Has it | Moved to its own folder when it finishes |
| Force recheck of downloaded data | Has it | Patched engine |
| Force reannounce to trackers | Has it | Patched engine |
| Force start (ignore the queue) | Has it (0.7) |  |
| Rename a torrent or its files | Missing | Engine lacks it |
| Copy magnet link, info-hash, name | Has it (0.7) |  |
| Save a torrent as a `.torrent` file | Has it (0.12) |  |
| Add, edit or remove a torrent's trackers | Missing | Engine lacks it |
| Create a `.torrent` from a file or folder | Has it (0.12) | Trackers in tiers, piece size, comment, private, start seeding |
| Delete the `.torrent` file after adding | Partly | Watch folder renames it instead |
| Several watch folders, each with its own options | Missing | One folder |

### Order and priority

| qBittorrent feature | RustTorrent | Note |
| --- | --- | --- |
| Switch in-order download off (rarest pieces first) | Missing | Engine lacks it. Rarest-first is healthier for a swarm with few seeders |
| File priority: high, normal, low | Has it | Patched engine; priority sets the order files download in |
| Download first and last pieces first | Has it | Always on |

### Limits

| qBittorrent feature | RustTorrent | Note |
| --- | --- | --- |
| Second set of speed limits with a quick switch, and a schedule for it | Has it (0.8) | "Slow mode", switched from the status bar or by its schedule |
| Speed limit per torrent | Has it | Patched engine; set in the details panel |
| Stop seeding after a time, or after a time without upload | Has it (0.8) |  |
| Limit on seeding torrents and on all active torrents | Has it (0.8) |  |
| Do not count slow torrents against the queue | Has it (0.8) |  |
| Limits on connections and upload slots | Partly (0.8) | Peers per torrent only; engine lacks the rest |

### Network and privacy

| qBittorrent feature | RustTorrent | Note |
| --- | --- | --- |
| Choose the listening port; switch UPnP, DHT, peer exchange on or off | Partly (0.9) | Port, UPnP and DHT; the engine has no switch for peer exchange. A change restarts the engine |
| Proxy (SOCKS5, HTTP) | Partly (0.9) | SOCKS5 only. Its password is kept as plain text in the settings file |
| IP block list | Has it (0.9) | Read when the engine starts |
| Ban a peer, add a peer by hand | Partly (0.9) | Ban only; engine lacks adding a peer |
| Protocol encryption setting | Missing | Engine lacks it |
| Anonymous mode | Missing | Engine lacks it |
| Local peer discovery | Has it (0.9) |  |
| Web seeds (HTTP sources) | Missing | Engine lacks it |
| BitTorrent v2 torrents | Missing | Engine lacks it |
| Super seeding | Missing | Engine lacks it |
| I2P (experimental in qBittorrent) | Missing | Not planned |

### Feedback and comfort

| qBittorrent feature | RustTorrent | Note |
| --- | --- | --- |
| Notification when a download finishes | Has it (0.10) | Not seen working: macOS refused permission to the locally signed app on the test Mac |
| Keep the Mac awake while transferring | Has it (0.10) | Not confirmed by measurement |
| Speed graph | Has it (0.10) |  |
| Piece map and availability bar | Partly (0.10) | Piece map; engine lacks per-piece availability |
| All-time statistics | Has it (0.10) |  |
| Log viewer in the app | Has it (0.10) |  |
| Run a program when a download finishes | Has it (0.10) | Run directly, not through a shell |
| Shut down or sleep when all downloads finish | Partly (0.10) | Can quit the app; does not shut down or sleep the Mac |
| Peer's country and client flags | Partly | Client name is shown |
| Filter the list by tracker | Has it (0.10) |  |
| Choose which columns are shown | Has it (0.10) |  |
| Menu-bar icon; keep running with the window closed | Missing | Left out of 0.10 |
| Warn when quitting during downloads | Has it (0.7) |  |
| Languages other than English | Missing | Not planned yet |
| Light theme | Has it (0.10) |  |
| Check for a new version | Not needed | `brew upgrade` does it |

### RSS and remote control

| qBittorrent feature | RustTorrent | Note |
| --- | --- | --- |
| RSS rules with regular expressions and episode filters ("S02E05 and later") | Has it (0.11) |  |
| RSS: read articles in the app, smart episode filter (skip repeats) | Has it (0.11) | Preview of a feed's items with a Download button each; repeats of an episode are skipped |
| Remote: everything the desktop window can do | Partly | Add link or file, pause, resume, remove, files, category, main speed limits |
| Remote: upload a `.torrent` file | Has it (0.11) |  |
| Remote: HTTPS | Has it (0.11) | Self-made certificate, so the browser warns once |
| Remote: API that other apps (phone apps, Sonarr, Radarr) can use | Missing | Left out of 0.11 |
| Search engine with plugins | Not planned | Plugins were ruled out |
| Email on completion, lock the window with a password | Not planned | |

## What 0.7 to 0.12 did

The six releases planned here were built together and published as one, 0.12.0.

**Built.** Everything marked "Has it" or "Partly" with a release number in the tables above.

**Planned but left out.**

- Menu-bar icon and running with the window closed (0.10).
- A copy of qBittorrent's API for phone apps and other tools (0.11).
- A separate folder for unfinished downloads (0.7). Built later, after 0.13.4.
- Binding the remote-control page to the chosen network interface (0.9).
- The remote page cannot see or set slow mode, the seeding limits or the queue limits.

**The engine checks failed for these**, so they are not built and are not faked: upload slots, a peer-exchange switch, adding a peer by hand, per-piece availability.

**Still needs an engine change.** A switch for in-order download, tracker editing, renaming, encryption setting, web seeds, BitTorrent v2, super seeding, and the list above. The route is to propose each change to librqbit, or to keep a patched copy. Recheck, reannounce, per-torrent limits and file priorities are now in a patched copy (manojknaikade/rqbit), offered upstream. The in-order switch needs a large change to the engine's piece picker and is left for later.

**Size.** The app is now 17 MB, 2 MB over its 15 MB target.
