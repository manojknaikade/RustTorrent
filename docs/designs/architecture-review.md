# Architecture review: speed, size, memory, features

Reviewed version 0.13.3 (commit `9f31d2e`) on Apple Silicon (10 cores), 21 November 2026. Sizes are in MiB (1 MiB = 1,048,576 bytes). That's how the README's "17 MB" lines up with the 18,336,912-byte binary.

## Do these three first

All three are now done; see "Results after the changes". So are the two fork changes, Z3 and Z4. The app is 14.12 MiB, measured. Seeding footprint was 52 MB, so the README's 104 MB was most likely RSS. M2 was tried and dropped.

1. **Build cold crates for size** (Z1). This is one block in the workspace `Cargo.toml`. It cuts the binary from 17.49 MiB to 16.78 MiB, measured. No other size change is this cheap.
2. **Measure seeding memory with `footprint` before you change anything for memory.** At idle with the window open, the footprint is 43 MB, measured. RSS is 90 MB, because RSS also counts shared system libraries. The README's "about 104 MB while seeding" doesn't say which number it is. If it was RSS, the 100 MB target may already be met. After measuring, lower the default peer count (M2). That's the one large memory lever in this repo.
3. **Drop the clipboard's image decoders on macOS** (Z2). This saves about 0.54 MB, estimated from measured symbol sizes, and needs no fork change. With Z1 the app would be about 16.3 MiB.

This repo alone can't get under 15 MiB. Getting there needs two fork changes: regex-lite in librqbit (Z3, about 0.57 MB) and ring instead of aws-lc (Z4, about 1.1 MB). Both are estimates. With all four the app would be about 14.7 MiB, also an estimate. **Measured after doing all four: 14.12 MiB.** Z3 saved 0.95 MiB and Z4 1.36 MiB.

## Results after the changes

Released in 0.13.4. 127 tests pass and clippy shows no warnings.

**Size: 17.49 → 14.12 MiB** (18,336,912 → 14,810,832 bytes, measured). Z1 with the quick wins gave 17,611,328 bytes. Z2 saved another 381,264 bytes, less than the 0.54 MB estimate, for 17,230,096 bytes. Z3 then gave 16,237,328 bytes and Z4 14,810,832 bytes.

| ID | Status | What was done, and the evidence |
| --- | --- | --- |
| Z1 | Done | Cold crates at `opt-level = "s"` in the workspace `Cargo.toml`. |
| Z2 | Done, macOS only | eframe's `clipboard` feature is on only for other systems. ⌘C, ⌘X and ⌘V are handled in `mac.rs` through `NSPasteboard`. 381,264 bytes smaller, measured. **Copy and paste of a magnet link still needs a test by hand.** |
| S1 | Not done | The finding was wrong (see "Biggest structural risks" #1). The idle snapshot already stays equal. |
| M6 | Done | The speed history is an `Arc<SpeedHistory>` changed with `Arc::make_mut`, and is pushed only when the speed changed. Test `history_says_when_a_speed_would_change_it`. |
| S2 | Done; the last part dropped | Settings, feeds and notices are `Arc`s, replaced only when they change. The name comes from the stored record. The hex hash is still made each tick, on purpose: the view keeps the key as its own `String` (`model.rs:70`), so a cache would still copy one string per torrent per tick. It would only save hex-encoding 20 bytes. |
| S3 | Done | `find` looks the torrent up by its 40-character hash through `Session::get`, with no scan. |
| S4 | Done | Names are lower-cased once per `rebuild()`, not per comparison. |
| S5 | Done | Sidebar counts are worked out in `rebuild()`. |
| M1, S6 | Done | New `logfile.rs`: the file stays open; past 1 MiB it becomes `rusttorrent.log.1`. Has a test. |
| M3 | Done | The feed body is read in chunks and stopped once it passes the cap. |
| M4 | Done | Pages are given back 5 s after start, and 3 s after a torrent is removed or the engine restarts. The 60 s timer stays. |
| M5 | Measured | `heap -s` while seeding: 14.3 MB live, of which 8.0 MB is untyped (Rust and C allocations). The rest belongs to macOS frameworks (Metal, AppKit, CoreFoundation). No font or cache change is worth making on this evidence. |
| uTP | Done | Network setting "Also connect over uTP (UDP)", off by default. It's greyed out while a proxy is on, because uTP would go around the proxy. Test `utp_is_used_only_when_on_and_there_is_no_proxy`. |
| M2 | Tried, dropped | See the runs below. Seeding footprint was the same, and 60 peers was slower in 2 of 3 runs. |
| Z3 | Done in the fork | Fork branch `feat/regex-lite`: `only_files_regex` uses `regex_lite::Regex`. `cargo tree -i regex` finds nothing now. 992,768 bytes smaller, measured, against the 0.57 MB estimate. regex-automata is still in the build through `bstr` in `librqbit-lsd`. |
| Z4 | Done in the fork and in core | Fork branch `feat/rustls-no-provider` adds a librqbit feature with reqwest's `rustls-no-provider`. Core uses rustls and tokio-rustls with `ring`, and `use_ring()` (`lib.rs`) installs ring's provider before any session or HTTP client is made. The certificate, the remote-control password hash and the fingerprint use ring. `cargo tree -i aws-lc-rs` finds nothing now. 1,426,496 bytes smaller, measured, against the 1.1 MB estimate. One HTTPS feed fetch through the app's client worked. **Two costs.** First, rustls with ring has no post-quantum key exchange: it offers X25519, P-256 and P-384 only (rustls 0.23.46 source). Second, ring's SHA-1 is plain Rust: 436 MiB/s against aws-lc's 2,464 MiB/s, measured. So torrent making (`make.rs`) now hashes with `crypto-hash`, which the engine already uses: 2,496 MiB/s on macOS, measured. Unoptimised, ring's PBKDF2 took 1.4 s against 76 ms optimised, which broke the login-limit test. ring is now built optimised in debug builds too. |
| Z5 | Not done, by choice | Names of old torrents that aren't UTF-8 would show garbled. About 0.3 MB, estimated, isn't worth that. |
| Z6 | Waiting | Nothing to do until accesskit and eframe move to objc2 0.6. |
| Feature 2 | Done, not released | `incomplete_dir` in Settings. A torrent starts there and is moved to its own folder when it finishes, through the existing move code (`moving.rs` `start_final_moves`). The finish notice and program wait until the move ends. It starts straight in its own folder when data for it is already there. Tests `start_folder_choice` and `torrent_files_are_relative_to_its_folder`. A loopback run with two cores and a local tracker (8 MB, two files) showed Downloading in the unfinished folder, then Moving, then the files in the download folder, matching the source, with "Finished downloading" after "Moved". Adding it again later went straight to the download folder. |
| Features 3–18 | Not started | Left for later releases, one at a time. |
| S7 | Tried, dropped | With 4 workers: 11 threads instead of 18, the same seeding footprint (50 MB), and higher seeding CPU (0.46% against 0.32%). One run only. No gain worth the recheck risk. |

Debian runs with the M2 and S7 builds (one run each unless marked):

| Run | Download | Seeding footprint | Seeding CPU | Threads |
| --- | --- | --- | --- | --- |
| 60 peers, first try | 29.4 s, stalled near the end | — | — | — |
| 60 peers, second try | 25.7 s | 52 MB | 0.2–0.6% | — |
| 60 peers, third try | 20.1 s | 50 MB | 0.32% | 18 |
| 60 peers, 4 workers | 22.3 s | 50 MB | 0.46% | 11 |
| 128 peers | 21.1 s | 49 MB | 0.34% | 17 |

**Download time swings with the swarm.** Two later runs of the final build took 40.4 s and 42.7 s. The speed held steady at about 24 MB/s, with no stall, so the swarm was slower than earlier in the day. To check that the code didn't cause it, the final build was run against a build from just before (the only difference is a setting read from an environment variable, which does nothing when unset):

| Pair | Ran first | Final build | Build from before | Seeding footprint (final / before) | Seeding CPU (final / before) |
| --- | --- | --- | --- | --- | --- |
| a | before | 39.6 s | 31.6 s | 53 / 53 MB | 0.51 / 0.33% |
| b | before | 47.2 s | 37.2 s | 52 / 54 MB | 0.35 / 2.21% |
| c | final | 21.3 s | 20.0 s | 50 / 50 MB | 0.34 / 0.43% |

In pairs a and b, the build that ran second was slower. In pair c, the final build ran first and both finished in about 20 s. So the gap came from run order and the swarm changing over the day, not from the code. Seeding CPU swings from run to run in both builds (0.33% to 2.21% for the same build).

Memory with the final build, measured while seeding: footprint 49–53 MB, seeding CPU 0.34–0.40%. That's the same as 0.13.3 (52 MB, 0.2–0.3%). The changes didn't lower seeding memory in a way these runs can see.

## 1. Architecture snapshot

```
 ┌───────────────────────── crates/app (rusttorrent) ──────────────────────────┐
 │ main thread: eframe/winit event loop + egui + wgpu (Metal)                  │
 │   App::ui()  — reads Arc<Snapshot>; rebuild() filters/sorts on ptr change   │
 │   comfort.rs — sparkline / stats: request_repaint_after(1 s)                │
 │ "memory" thread — malloc_zone_pressure_relief every 60 s (mac.rs:115)       │
 └──────────┬───────────────────────────────────────────▲──────────────────────┘
            │ Command (unbounded mpsc)                   │ Arc<Mutex<Arc<Snapshot>>>
            ▼                                            │ + on_change → request_repaint
 ┌──────────────────────── crates/core (rusttorrent-core) ─────────────────────┐
 │ "rusttorrent-core" thread → tokio multi-thread runtime (default: 1/core)    │
 │   Worker loop: commands → tick() every 1 s → full Snapshot rebuild,         │
 │                deep compare, swap (engine.rs:1499–1785)                     │
 │   ├─ librqbit Session (fork 5a049012): peers, DHT, UPnP, LSD, trackers,     │
 │   │    fastresume JSON in data_dir/session, blocklist from file:// URL      │
 │   ├─ feeds: reqwest fetch → quick-xml → regex-lite rules → add              │
 │   ├─ remote: hand-written HTTP/1.1 server, optional TLS (rustls, ring)      │
 │   ├─ store.rs: JSON, atomic (tmp + fsync + rename), on events + 30 s        │
 │   └─ spawn_blocking: blocklist prep, file reads, .torrent making            │
 └─────────────────────────────────────────────────────────────────────────────┘
```

How state moves:
- The UI sends `Command`s over an unbounded channel. The core thread runs its own tokio runtime (`engine.rs:327`), built with `new_multi_thread()` and no worker count.
- `Worker::tick()` runs every second and after each batch of commands. It builds a whole new `Snapshot`, compares it with the old one field by field (`engine.rs:1780`), and swaps it in only if something changed. `on_change` then asks egui to repaint.
- The UI checks `Arc::ptr_eq` and calls `rebuild()` (filter and sort) only when the snapshot changed. Tables are virtualised.
- Settings and state are saved as JSON with atomic writes (`store.rs:262–271`). Saves happen on events, and every 30 s if something changed (`engine.rs:198`).

Biggest structural risks:
1. ~~**The snapshot changes every second, even when nothing happens.**~~ **Wrong, corrected after the review:** `SpeedHistory::push` returns early when the speed hasn't changed (`comfort.rs:93`), and the test `history_keeps_changes_only` checks that. So at idle the snapshot stays equal and the UI doesn't repaint. The history did get copied into every snapshot (`engine.rs:1767`); that was M6, now fixed.
2. **`tick()` is one 290-line function** that both polls librqbit and builds every view. Speed work on it, such as caching or splitting out the graph, is risky without tests of the snapshot. `engine.rs` has no tests for `tick()`.
3. **Most of the remaining size wins and most missing features depend on the fork.** Every change marked "needs fork change" widens the gap with upstream librqbit.

What is already good, so these are not findings:
- atomic saves,
- virtualised tables,
- regexes compiled once (`feeds.rs:63`),
- blocklist lookups done inside librqbit,
- metadata cached once per torrent (`meta_for`, `engine.rs:1468`),
- blocking work run on `spawn_blocking`,
- notices capped at 50 (`engine.rs:199`, `:471`),
- freed pages returned every 60 s (`mac.rs:115`).

## 2. Measurements

| What | Command | Result | Couldn't measure, and why |
| --- | --- | --- | --- |
| Binary size, 0.13.3 | `stat -f %z target/release/rusttorrent` | 18,336,912 bytes (17.49 MiB) | — |
| Size by crate | Unstripped release build in a scratch copy, then `nm -n -m` and a script that splits symbol spans by crate | Top: aws-lc-sys 1.61 MB, naga 1.16, egui 0.98, wgpu_core 0.71, tokio 0.60, core 0.57, rustls 0.55, harfrust 0.50, app 0.49, read_fonts 0.48, librqbit 0.40, skrifa 0.37, librqbit_utp 0.30, regex_automata 0.28, regex_syntax 0.18, tracing 0.16 | `cargo bloat` isn't installed. The prompt says to ask before installing it, so I used `nm` instead. The `encoding_rs` total (0.92 MB) is too high: embedded font data (Hack, Ubuntu-Light and the egui icon font) sits right after it and gets counted with it. Its real size is about 0.3 MB. |
| Unwind tables | `size -m` on the unstripped build | 1.77 MB | — |
| Duplicate crates | `cargo tree -d` | objc2 0.5 and 0.6 (0.5 comes through accesskit_macos); two versions each of block2, bitflags and core-foundation; getrandom, rand and rand_core 0.9 (from governor and utp); hashbrown 0.14 (from dashmap); hex 0.3 and 0.4; rustc-hash 1 and 2; thiserror 1 (from tokio-socks); untrusted 0.7 | — |
| Tracing off at compile time | Scratch copy with `tracing = { version = "0.1", default-features = false, features = ["release_max_level_off"] }` added to core, then `cargo build --release` | 18,320,320 bytes: **16.6 KB smaller**, measured | — |
| Tracing off, plus cold crates at `opt-level = "s"` | Same copy, plus `[profile.release.package.X] opt-level = "s"` for naga, wgpu-core, wgpu-hal, encoding_rs, image, tiff, zune-jpeg, regex-automata, regex-syntax, serde_json, chrono, quick-xml, reqwest, hyper, hyper-util, accesskit_consumer, accesskit_macos, rfd and winit | 17,594,720 bytes (16.78 MiB): **742 KB smaller** than 0.13.3, measured | The effect on speed wasn't measured. None of these crates is on the download path. |
| Idle memory, window open, no torrents | `footprint <pid>` and `vmmap --summary <pid>`, taken 22 s after launch | Footprint 43 MB (peak 51.2 MB). Breakdown: small mallocs 19 MB, IOSurface 7.1 MB, graphics 4 MB, CG Image 2.1 MB, `__DATA*` 4 MB, stacks 0.9 MB. RSS 90 MB. | `heap` wasn't run, so the 19 MB of small mallocs isn't split by owner yet. |
| Idle CPU | `sample <pid> 15`, `ps -o %cpu` | 0.0%. Every thread waits in the kernel. Out of about 12,000 samples per thread, one landed in app code. | — |
| Threads | `ps -M`, and thread names in the sample | 18 threads: 13 `tokio-rt-worker`, `rusttorrent-core`, `memory`, main, NSEventThread and dispatch threads | — |
| Memory and CPU while downloading and seeding; Debian download time (0.13.3) | Script in the scratchpad: a separate data folder, the Debian 13.8.0 netinst torrent (758 MiB), 128 peers. Every 1 s while downloading and every 5 s for 2 minutes of seeding: `footprint`, `ps -o rss,time`, `du` of the download folder. Then `footprint`, `vmmap --summary`, `heap -s` and `ps -M` while seeding. | Done 22.3 s after launch. Footprint 51 MB when done (peak 56 MB), 52 MB while seeding. RSS 96–97 MB while downloading. Seeding CPU 0.2–0.3%. 18 threads. | Run once, before the changes. Later runs show the download time swings a lot with the swarm (see "Results after the changes"). |
| Startup time, frame time | — | — | Not run. These need a GUI test harness. |

## 3. Findings

### Speed

| ID | Title | Evidence | Gain | Effort | Risk | Fork? | Change |
| --- | --- | --- | --- | --- | --- | --- | --- |
| S1 | ~~Snapshot changes every second, so the UI repaints and re-sorts every second~~ | **Wrong.** `SpeedHistory::push` skips a sample when the speed is unchanged (`comfort.rs:93`), so the idle snapshot stays equal. | None | — | — | No | Nothing to do. The copy it pointed at is M6. |
| S2 | `tick()` rebuilds the same strings for every torrent every second | From `engine.rs:1499` on: `info_hash().as_string()` and `handle.name()` are called several times per torrent. `file_progress`, `only_files`, settings, feeds and notices are copied every tick. | Small, estimated. It only matters with hundreds of torrents. | S | Low | No | Cache the hex hash and name per torrent id in a `HashMap` filled when a torrent is added. Keep notices, feeds and settings in `Arc`s that are replaced only when they change, so the snapshot shares them instead of copying. |
| S3 | `find` scans every handle and allocates a string for each | `engine.rs:495` | Small, estimated. It runs for every command, so it adds up when you act on many torrents at once. | S | Low | No | Keep a `HashMap<String, Handle>` from key to handle. Update it on add and remove. |
| S4 | Sorting by name allocates two strings per comparison | `app.rs:393`: `a.name.to_lowercase().cmp(&b.name.to_lowercase())` | Estimated: sorting 1,000 torrents makes about 20,000 allocations, each time the snapshot changes. | S | Low | No | Use `sort_by_cached_key`, or store each name in lower case once in the view. |
| S5 | Sidebar counts scan all torrents every frame | `app.rs:518` and `:548` (`.filter().count()`); `names_in_use` in `rebuild()` | Small, estimated | S | Low | No | Work out the counts in `rebuild()`, which already runs only when the snapshot changes, and store them. |
| S6 | Each notice opens the log file again | `engine.rs:457–468` | Small, estimated: one `open` system call per notice | S | Low | No | Keep the file open in the `Worker`. Do this together with log rotation (M1). |
| S7 | tokio starts one worker thread per core | `engine.rs:327`. 13 `tokio-rt-worker` threads seen on a 10-core Mac, measured. | **Tried after the review:** 4 workers gave 11 threads instead of 18, the same seeding footprint (50 MB) and higher seeding CPU (0.46% against 0.32%), one run. | S | Med | No | **Dropped.** No gain worth the risk of slower hashing during a recheck. |

### Size

0.13.3 is 17.49 MiB. Getting under 15 MiB means saving 2.49 MiB (2.61 MB).

| ID | Title | Evidence | Gain | Effort | Risk | Fork? | Change |
| --- | --- | --- | --- | --- | --- | --- | --- |
| Z1 | Build cold crates for size | Measurement table. naga (1.16 MB) and wgpu_core (0.71 MB) mostly run once at startup. The rest of the list (image decoders, regex, JSON, HTTP, accesskit, rfd, winit) isn't on the download path. | 742 KB, measured (17.49 → 16.78 MiB). 16.6 KB of that came from switching tracing off, so cold crates alone save about 725 KB. | S | Low | No | Add `[profile.release.package.<name>] opt-level = "s"` for each crate in the measurement table, in the workspace `Cargo.toml`. Keep tokio, librqbit, rustls, aws-lc, egui and epaint at `opt-level = 3`. Check by hand that startup and scrolling a long list feel the same. |
| Z2 | The clipboard pulls in image decoders | egui-winit's `clipboard` feature depends on arboard, which always has `image-data` on. That brings in tiff, image, zune_jpeg, fax, png, weezl and miniz: about 0.54 MB of measured symbols. The app only copies and pastes text (`app.rs:800`, `:1249`). | About 0.54 MB, estimated from measured symbol spans | M | Low | No | On macOS only, drop eframe's `clipboard` feature with a target-specific dependency. Do text copy and paste with `NSPasteboard` through objc2, which is already a dependency: handle `OutputCommand::CopyText`, and turn ⌘V into `Event::Paste`. Pasting a magnet link must keep working; test that by hand. |
| Z3 | librqbit uses the full `regex` crate for one call | Fork `session.rs:187`: `regex::Regex::new(filename_re)` for `only_files_regex`, which the app never sets. regex_automata 0.28 MB, regex_syntax 0.18 MB, aho_corasick 0.12 MB. | About 0.57 MB, estimated from measured spans | S | Low | Yes | In the fork, switch that call to `regex-lite`, which the app already ships. First check that nothing else in the dependency tree pulls in `regex`. |
| Z4 | Use one crypto library: ring instead of aws-lc | aws-lc-sys is the largest single item, at 1.61 MB. It comes from three places: reqwest's default TLS (through librqbit's `default-tls`), rustls and tokio-rustls, and core's own use in `remote/tls.rs` and `remote/mod.rs:30` (pbkdf2, ECDSA, digest, random). ring offers all of these. | About 1.1 MB, estimated. That assumes ring adds about 0.5 MB, which is a guess. | M | Med | Yes | In the fork, add a librqbit feature that uses reqwest's `rustls-no-provider`. In core, depend on rustls and tokio-rustls with `ring`, install ring's provider at startup, and port `tls.rs` and the pbkdf2 code to ring's matching API. The cost is that rustls with ring has no post-quantum hybrid key exchange. That's acceptable for tracker and feed HTTPS, but it's your policy call. |
| Z5 | chardetng and encoding_rs | `librqbit_core/src/torrent_metainfo.rs:387` guesses the encoding of torrent names that aren't UTF-8 | About 0.3 MB, estimated | S | Med | Yes | Make it an optional feature in the fork. Do this only if you accept garbled names on old torrents that aren't UTF-8. Low priority. |
| Z6 | Duplicate crate versions | `cargo tree -d` (see the measurement table) | Under 0.2 MB, a guess. Most of the duplicates are small. | S each | Low | No | Wait for accesskit and eframe to move to objc2 0.6. Don't pin or patch crates for this. |

### Memory

| ID | Title | Evidence | Gain | Effort | Risk | Fork? | Change |
| --- | --- | --- | --- | --- | --- | --- | --- |
| M1 | The log file grows without limit | `engine.rs:457–468` appends to `rusttorrent.log` forever. Nothing trims it. | Disk, not RAM. Today there is no limit. | S | Low | No | When the file passes 1 MB, rename it to `rusttorrent.log.1` and start a new one. Keep one old file. |
| M2 | 128 peers per torrent by default | `peers_per_torrent: 0` (`limits.rs:78`) becomes `None` (`engine.rs:604`), and librqbit then uses 128 (`torrent_state/live/mod.rs:276`). Each peer has a 32 KiB read buffer (`read_buf.rs:13`) and a write buffer of `MAX_MSG_LEN`, just over 16 KiB (`peer_connection.rs:122`). | Estimated: about 6 MB per busy torrent at 128 peers, 3 MB at 60. **Measured after the review:** seeding footprint 50–52 MB at 60 peers against 49 MB at 128, so no saving shows. The swarm likely never fills 128 slots. 60 peers was over 25 s in 2 of 3 runs. | S | Med | No | **Dropped.** Kept 128. |
| M3 | The feed size cap is checked only after the whole body is read | `engine.rs:1875–1878` | Peak memory is limited only by how much the server sends | S | Low | No | Reject early if `content_length()` is over 8 MiB. Then read with `response.chunk()` and stop once the cap is passed. |
| M4 | Freed pages go back to the system only once a minute | `mac.rs:115–118`. Peak footprint was 51.2 MB and the current footprint 42.7 MB, measured, so about 8 MB was given back after startup. | Up to about 8 MB returned sooner after bursts (measured at startup) | S | Low | No | Also call `malloc_zone_pressure_relief` once 5 s after startup, after torrents are removed, and after the engine restarts. Keep the 60 s timer. |
| M5 | 19 MB of small mallocs at idle, not yet traced to an owner | `footprint`: small mallocs, at 19 MB, are the largest item at idle | Unknown until measured | S (to measure) | Low | No | Run `heap -s <pid>` and `malloc_history` at idle. That splits the 19 MB between egui's font atlas and glyph cache, the system font collection, tokio and librqbit. Decide on any font or cache change only after that. |
| M6 | The speed history is copied into every snapshot | `engine.rs:1767`: 300 samples (`HISTORY_SECS=300`) of 24 bytes each is about 7 KB, copied every second | Only allocation churn, about 7 KB per second, estimated | S | Low | No | S1 fixes this. Use one `Arc<SpeedHistory>` with `Arc::make_mut`. |

## 4. Features

Ranked by value divided by effort. The "librqbit" column says whether the engine supports the feature today, needs a fork change, or isn't involved.

| Rank | Feature | User problem, and who asks | librqbit | Effort | Size, memory | Depends on |
| --- | --- | --- | --- | --- | --- | --- |
| 1 | uTP switch (TCP and uTP) | Better connections and fewer stalls behind NAT; many peers only talk uTP. People moving from qBittorrent or Transmission ask for it. | Supported: `ListenerMode::TcpAndUtp` (`listen.rs:25`). The code is already in the binary (librqbit_utp, 0.30 MB). Upstream's default is TCP only, with the comment "once uTP is stable". | S | Size: none. Memory: one UDP socket plus state per connection. | A setting in `network.rs:201`, off by default |
| 2 | Separate folder for unfinished downloads | Keeps half-finished files out of the media library. A listed qBittorrent gap. | Supported: add the torrent to one folder, then move it when it finishes with the existing move code (`relocate.rs`) | S–M | None | — |
| 3 | Disk-space guard | A full disk stalls or breaks downloads with no clear message | Not involved: an in-repo `statvfs` check before adding and on each tick, then pause and show a notice | S | None | — |
| 4 | Play while downloading | Watch a video before it finishes. A common reason people pick another client. | Supported: `ManagedTorrent::stream()` (`torrent_state/streaming.rs:337`) downloads the pieces being read first | M | Size: tiny. It reuses the hand-written HTTP server in `remote/`, on 127.0.0.1, and opens the URL in the default player. Memory: one stream buffer. | — |
| 5 | Remote page catches up with the window (slow mode, seeding and queue limits) | The remote page can't do what the window can (qbittorrent-gap.md:118) | Not involved: in-repo work | S–M | None | — |
| 6 | VoiceOver labels | Blind and low-vision users can't use the piece strip, the speed graph or icon-only buttons | Not involved: accesskit is already on | S–M | None | — |
| 7 | Menu-bar icon; keep running with the window closed | A seeding app that stays out of the way. Left out of 0.10. | Not involved: in-repo `NSStatusItem` through objc2 | M | Size: small. Memory at idle could drop if the wgpu surface is freed while the window is hidden; IOSurface is 7 MB at idle (measured), but the saving is unmeasured. | — |
| 8 | Sleep or shut down when all downloads finish | Overnight downloads (qbittorrent-gap.md:83) | Not involved: an in-repo IOKit or `pmset` call | S | None | — |
| 9 | Proxy password in the Keychain | The password is stored as plain text today (qbittorrent-gap.md:61) | Not involved: Security framework through objc2 | S–M | Size: small | — |
| 10 | qBittorrent-compatible Web API | Phone apps, Sonarr and Radarr need it | Not involved: in-repo, on top of the existing remote server and serde_json | L | Size: about 0.1–0.2 MB, a guess. Memory: none when idle. | Rank 5 |
| 11 | Import from Transmission or qBittorrent | Switch clients without re-checking everything | Partly: add each torrent with its existing folder, and librqbit rechecks the data. Their `.fastresume` files are bencode, which the fork can already read. | M | None | — |
| 12 | Several watch folders | Different categories from different drop folders | Not involved: in-repo | S–M | None | — |
| 13 | NAT-PMP alongside UPnP | Routers without UPnP, such as many Apple and ISP routers | Not in librqbit. NAT-PMP is a small UDP protocol you could write in core. | M | Size: small | — **Done:** `natpmp.rs` in core, a setting under Network |
| 14 | Reload the blocklist and network settings without restarting the engine | Today every change restarts the engine (qbittorrent-gap.md:60, 62) | Needs a fork change: the blocklist and listener are fixed when the session starts | M–L | None | Fork. **Done for the blocklist:** a blocklist-only change is applied to the running session. Other network changes still restart the engine. |
| 15 | Tracker editing, rename, encryption setting, web seeds, BitTorrent v2 | Listed qBittorrent gaps | Each one needs a fork change | L each | Small to medium | Fork. **Tracker editing done** (Trackers tab). Rename, encryption, web seeds and v2 not started. |
| 16 | Switch for in-order download (rarest first instead) | Healthier swarms when there are few seeders | Needs a large fork change to the piece picker (qbittorrent-gap.md:122) | L | None | Fork. Skipped. |
| 17 | Shortcuts or AppleScript support | Automation for power users | Not involved. AppleScript needs a scripting dictionary and Apple Event handlers through objc2. Shortcuts (App Intents) needs Swift. | M–L | Small | — **AppleScript done:** `add torrent`, `pause torrents`, `resume torrents`, `torrent list`. Shortcuts not done. |
| 18 | Spotlight or Quick Look for `.torrent` files | Look inside a `.torrent` before adding it | Not involved, but it needs a separate Quick Look extension bundle | L | A second binary in the bundle | — Skipped. |

## 5. Roadmap

### Quick wins (one sitting each, low risk)
Z1 (cold crates at `opt-level = "s"`), S4 (cached sort key), S5 (counts in `rebuild()`), M1 with S6 (log rotation, file kept open), M3 (streamed feed cap), M4 (extra page returns), M5 (heap breakdown), and the uTP switch (feature 1).
- **Size:** 17.49 → 16.80 MiB (17,611,328 bytes), measured. **Done.**
- **Memory:** idle stays at about 43 MB footprint. Seeding: 49–53 MB footprint, measured, the same as 0.13.3.

### Next release (medium effort, clear payoff)
Z2 (macOS clipboard), M6 (shared speed history), S2 and S3 (tick caches), and features 2–8. **Z2, M6, S2, S3 and features 2–8 are done; feature 9 (the Keychain) was done with them.** S1 was wrong; M2 and S7 were tried and dropped.
- **Size:** 16.43 MiB, measured. Still over 15 MiB.
- **Memory:** no change measured. The M2 saving didn't show up in the runs. The menu-bar mode (feature 7) keeps the wgpu surface while the window is hidden, so it saves no memory; freeing the surface is left out because the saving is unmeasured.

### Later (large, or needs fork work)
Z3 (regex-lite in the fork), Z4 (ring in the fork and in core), Z5 (optional chardetng), the qBittorrent-compatible API, import from other clients, NAT-PMP, and features 14–18. **Z3 and Z4 are done. Z5 was skipped by choice. Features 10–13 are done, with the blocklist part of 14, tracker editing from 15 and AppleScript from 17. Features 16 and 18 were skipped. The rest of 14, 15 and 17 is not started.**
- **Size:** 16.43 → 15.49 MiB after Z3 and 14.12 MiB after Z4, measured. Under 15 MiB. Z5 might take off about 0.3 MiB more, estimated.
- **Memory:** no change expected.

## 6. Things not to do

| Idea | Why not |
| --- | --- |
| Switch off tracing at compile time (`release_max_level_off`) | Measured: it saves 16.6 KB. The app installs no tracing subscriber, so LTO already removes almost all of it. Not worth another dependency line, and it would silently hide librqbit's logs if a log viewer ever used them. |
| Go back to the OpenGL (glow) renderer | Tried in 0.13.2. It used about 11 MB more memory at idle and was reverted in 0.13.3. wgpu costs about 2.3 MB of binary (naga 1.16, wgpu_core 0.71, wgpu_hal 0.19, wgpu 0.13, egui_wgpu 0.08 MB). Z1–Z4 win back more than that without the memory cost. |
| `panic = "abort"` | It would save about 1.77 MB of unwind tables, measured. But unwinding is a stated choice so that one failing task can't take down the app, and the other items reach the size target without it. |
| Make axum optional in the fork | LTO already removes it: about 0 KB in the binary, measured. |
| Drop `system_fonts` | It supplies the fallback fonts for CJK and other scripts in torrent names. Without it those names show as boxes. |
| Drop `default_fonts` | It would save about 0.67 MB, measured (the Hack, Ubuntu-Light and icon fonts). But it changes the look and relies on finding a good system font. Consider it only after M5, with a system font measured as the main font. |
| Compact JSON instead of pretty JSON in `store.rs` | Saves happen at most every 30 s, and only when something changed. A readable settings file helps support more than the bytes would. |
| mimalloc or another allocator | Idle CPU is already 0%, and the 60 s page return handles fragmentation on macOS. A new allocator adds size and makes `malloc_zone_pressure_relief` useless. Reconsider only if the seeding measurement shows fragmentation. |
| `-C target-cpu=apple-m1` | The hot hashing (piece SHA-1) runs in the system's crypto library through crypto-hash, not in Rust code, so the flag would not reach it. It would also rule out Intel builds. |
| Search plugins | Ruled out in qbittorrent-gap.md:103. They need a Python runtime or a plugin host, which breaks "one binary". |
| Check for updates in the app | `brew upgrade` already does this (qbittorrent-gap.md:91). |
| Replace librqbit | The fork already carries recheck, reannounce, per-torrent limits and file priorities. Each gap above is a contained fork change, which is much cheaper than replacing the engine. |
