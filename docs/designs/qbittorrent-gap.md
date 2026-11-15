# What qBittorrent has that RustTorrent lacks

* **Date:** 2026-11-15
* **Compared:** RustTorrent 0.6 against qBittorrent 5.x, from qBittorrent's documented feature list. qBittorrent was not installed for this comparison.

"Engine" below means [librqbit](https://github.com/ikatson/rqbit) 9.1, the library that does the transfers. Where a row says "engine check", the engine's source mentions the ability but nobody has yet tried it from RustTorrent. Where it says "engine lacks it", the feature needs a change inside the engine or a different engine.

## Already level

Magnet links and `.torrent` files, pause/resume/remove, choosing files, global speed limits, download queue, ratio limit, time-of-day schedule, categories and tags, watch folder, RSS auto-download, network interface binding, remote control from a browser, DHT, peer exchange, automatic port forwarding (UPnP), resume after restart, search and status filters, peers and trackers views.

Better than qBittorrent today: in-order download is always on and works; all details are in the main window; the app is 16 MB and installs with one Homebrew command on Apple Silicon.

## Missing

### Adding and managing torrents

| qBittorrent feature | RustTorrent | Note |
| --- | --- | --- |
| Add dialog: choose folder, category, files and "start paused" before the download begins | Missing | Files can be chosen only after adding |
| Move a torrent's data to another folder | Missing | Engine check |
| Folder per category | Missing | |
| Separate folder for unfinished downloads | Missing | |
| Force recheck of downloaded data | Missing | Engine check |
| Force reannounce to trackers | Missing | Engine check |
| Force start (ignore the queue) | Missing | |
| Rename a torrent or its files | Missing | Engine lacks it |
| Copy magnet link, info-hash, name | Missing | |
| Save a torrent as a `.torrent` file | Missing | |
| Add, edit or remove a torrent's trackers | Missing | Engine lacks it |
| Create a `.torrent` from a file or folder | Missing | Engine check |
| Delete the `.torrent` file after adding | Partly | Watch folder renames it instead |
| Several watch folders, each with its own options | Missing | One folder |

### Order and priority

| qBittorrent feature | RustTorrent | Note |
| --- | --- | --- |
| Switch in-order download off (rarest pieces first) | Missing | Engine lacks it. Rarest-first is healthier for a swarm with few seeders |
| File priority: high, normal, low | Missing | Engine lacks it; files are only on or off |
| Download first and last pieces first | Has it | Always on |

### Limits

| qBittorrent feature | RustTorrent | Note |
| --- | --- | --- |
| Second set of speed limits with a quick switch, and a schedule for it | Missing | The schedule today is all-or-nothing |
| Speed limit per torrent | Missing | Engine check |
| Stop seeding after a time, or after a time without upload | Missing | |
| Limit on seeding torrents and on all active torrents | Missing | Only downloads are limited |
| Do not count slow torrents against the queue | Missing | |
| Limits on connections and upload slots | Missing | Engine check |

### Network and privacy

| qBittorrent feature | RustTorrent | Note |
| --- | --- | --- |
| Choose the listening port; switch UPnP, DHT, peer exchange on or off | Missing | Engine check; all are on with defaults today |
| Proxy (SOCKS5, HTTP) | Missing | Engine check |
| IP block list | Missing | Engine check |
| Ban a peer, add a peer by hand | Missing | |
| Protocol encryption setting | Missing | Engine lacks it |
| Anonymous mode | Missing | Engine lacks it |
| Local peer discovery | Missing | Engine lacks it |
| Web seeds (HTTP sources) | Missing | Engine lacks it |
| BitTorrent v2 torrents | Missing | Engine lacks it |
| Super seeding | Missing | Engine lacks it |
| I2P (experimental in qBittorrent) | Missing | Not planned |

### Feedback and comfort

| qBittorrent feature | RustTorrent | Note |
| --- | --- | --- |
| Notification when a download finishes | Missing | |
| Keep the Mac awake while transferring | Missing | |
| Speed graph | Missing | |
| Piece map and availability bar | Missing | |
| All-time statistics | Missing | |
| Log viewer in the app | Missing | The log is a file |
| Run a program when a download finishes | Missing | |
| Shut down or sleep when all downloads finish | Missing | |
| Peer's country and client flags | Partly | Client name is shown |
| Filter the list by tracker | Missing | |
| Choose which columns are shown | Missing | |
| Menu-bar icon; keep running with the window closed | Missing | |
| Warn when quitting during downloads | Missing | |
| Languages other than English | Missing | Not planned yet |
| Light theme | Missing | Dark only |
| Check for a new version | Not needed | `brew upgrade` does it |

### RSS and remote control

| qBittorrent feature | RustTorrent | Note |
| --- | --- | --- |
| RSS rules with regular expressions and episode filters ("S02E05 and later") | Missing | Word lists only |
| RSS: read articles in the app, smart episode filter (skip repeats) | Missing | |
| Remote: everything the desktop window can do | Partly | Add link, pause, resume, remove, limits |
| Remote: upload a `.torrent` file | Missing | |
| Remote: HTTPS | Missing | Unencrypted today |
| Remote: API that other apps (phone apps, Sonarr, Radarr) can use | Missing | qBittorrent's API is widely supported |
| Search engine with plugins | Not planned | Plugins were ruled out |
| Email on completion, lock the window with a password | Not planned | |

## Plan

Ordered by how often the missing piece is felt in daily use, and by risk: things that need only RustTorrent's own code come first; things that need an engine check come with a fallback; things the engine lacks come last.

**0.7, managing torrents.** Add dialog (folder, category, files, start paused). Copy magnet link, hash and name. Force start. Folder per category. Folder for unfinished downloads. Force recheck and reannounce, and move data, if the engine checks pass; otherwise move data is done by pausing, moving the files and re-adding. Warn when quitting during downloads.

**0.8, limits.** Second set of speed limits with a toolbar switch and its own schedule. Seeding time limits. Limits on seeding and on all active torrents. Per-torrent speed limits and connection limits if the engine checks pass.

**0.9, network.** Listening port; switches for UPnP, DHT and peer exchange; SOCKS5 proxy; IP block list. Each depends on an engine check. Bind the remote-control page to the chosen interface too.

**0.10, feedback.** Finish notification. Keep the Mac awake while transferring. Speed graph. Piece map. Log viewer. Statistics. Tracker filter. Column chooser. Menu-bar icon. Light theme.

**0.11, RSS and remote.** Regular expressions and episode filters in feed rules. Remote page: `.torrent` upload, file choice, categories, HTTPS with a self-made certificate. Decide then whether to copy qBittorrent's API so that existing phone apps work.

**0.12, making torrents.** Create a `.torrent` from a file or folder; save a torrent as a `.torrent` file.

**Needs an engine change (no release assigned).** A switch for in-order download, file priorities, tracker editing, renaming, encryption setting, local peer discovery, web seeds, BitTorrent v2, super seeding. The route is to propose each change to librqbit, or to keep a patched copy. Before 0.9, decide which of these matter enough to justify that work; the in-order switch and file priorities are the two most asked for in other clients.

**Size.** The app is 1 MB over its 15 MB target, and every release above adds code. The speed graph and piece map are drawn with the existing drawing library and add little; HTTPS for the remote page reuses the encryption library already included.
