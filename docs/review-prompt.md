# Architecture review prompt

Paste everything below the line into a fresh Claude Code session opened at the repository root.

---

You are reviewing **RustTorrent**, a native torrent client for macOS (also builds for Windows and Linux), written in Rust. Your job is to review its architecture and produce a ranked, evidence-backed set of recommendations in four areas: **speed**, **app size**, **memory use**, and **new features**.

Work read-only until the report is written. Do not change code, open PRs, or push anything. Measure where you can; where you can't, say the claim is unmeasured.

## 1. What the project is

- Workspace with two crates:
  - `crates/core` (`rusttorrent-core`): rules, storage and the engine adapter, with no UI code. Main files: `engine.rs` (~1,900 lines), `remote/` (built-in web remote with optional TLS), `rules.rs`, `feeds.rs`, `network.rs`, `store.rs`, `comfort.rs`, `episodes.rs`, `blocklist.rs`, `make.rs`, `arbiter.rs`, `limits.rs`, `priority.rs`, `intake.rs`, `relocate.rs`.
  - `crates/app` (`rusttorrent`): the egui/eframe UI, using the wgpu renderer (Metal on macOS). Main file: `app.rs` (~2,400 lines), plus `add_dialog.rs`, `network.rs`, `comfort.rs`, `feed_ui.rs`, `make.rs`, `mac.rs`/`mac_extras.rs` (objc2), `app/limits.rs`.
- Engine: `librqbit 9.1.0`, patched through `[patch.crates-io]` to the fork `manojknaikade/rqbit` (adds recheck, reannounce, per-torrent rate limits, file priorities). Upstream PRs for these are open. **Treat the fork as a constraint.** Changes inside librqbit are allowed as recommendations but must be marked "needs fork change". Never suggest opening or updating upstream PRs.
- Other key dependencies: tokio (multi-thread), reqwest 0.13 (rustls + socks), rustls/tokio-rustls with aws-lc-rs, serde/serde_json, chrono, regex-lite, rfd, egui_extras.
- Release profile: `opt-level = 3`, `lto = "fat"`, `codegen-units = 1`, `strip = true`, panics unwind on purpose so one failing task can't take down the app.
- Design docs: `README.md`, `concepts_brief.md`, `docs/designs/rusttorrent.md`, `docs/designs/qbittorrent-gap.md`. Packaging: `scripts/bundle-macos.sh`. CI: `.github/workflows/ci.yml`. `spikes/` holds throwaway experiments and is outside the workspace.
- Recent history to respect (see `git log`): 0.13.2 switched to the OpenGL (glow) renderer for a smaller app. 0.13.3 went back to wgpu because glow used about 11 MB more memory at idle, and it now returns freed pages to the OS. Don't re-suggest a trade-off that was already tried unless you have new evidence.

Stated goals and current numbers (README, measured on Apple Silicon with 0.12; re-measure on the current build):

| Metric | Measured | Target |
| --- | --- | --- |
| App on disk | 17 MB (8.5 MB zipped) | under 15 MB |
| Memory, one finished 735 MB torrent seeding | ~104 MB | under 100 MB |
| CPU at that time | ~0.5% | as low as possible |
| 735 MB Debian download | under 25 s | no regression |

Product principles: one binary, no web engine, everything about a torrent in the main window, small and fast.

## 2. How to work

1. **Read first.** Read the design docs, both `Cargo.toml` files, `Cargo.lock` (dependency count, duplicate versions), `lib.rs`, `main.rs`, then `engine.rs` and `app.rs` in full. Skim the rest.
2. **Map the architecture.** Write down:
   - The thread and task layout: the UI thread, the tokio runtime and its worker count, blocking pools, and timers.
   - How state moves between engine and UI: channels, `Arc<Mutex>`/`RwLock`, polling or push, and how often.
   - The repaint strategy: when egui repaints (continuous, on an interval, or on events) and what each frame recomputes.
   - Persistence: what `store.rs` writes, how often, the format, and whether writes are atomic.
   - Network surfaces: engine, feeds, remote control, blocklist download, interface binding.
3. **Measure, don't guess.** Use these where possible and include the commands you ran:
   - Size: `cargo build --release -p rusttorrent`, then `ls -l` on the binary and `cargo bloat --release --crates -n 30` and `--filter` on the top crates (install with `cargo install cargo-bloat` if it's missing; ask first). Also `cargo tree -d` for duplicate dependencies and `cargo tree -e features` for features you may not need.
   - Memory: run the app, add the Debian torrent, let it finish and seed, then check with `footprint <pid>`, `vmmap --summary <pid>`, and `heap <pid>` on macOS. Record idle (no torrents), active download, and seeding.
   - CPU and frame cost: `sample <pid> 10` while idle and while downloading. Look for repaints at idle, polling loops, and per-frame allocation or sorting.
   - If you can't run the GUI, say so and fall back to static analysis, marking each finding as unmeasured.
4. **Check each hypothesis against the code** before reporting it. Quote `file:line`.

## 3. What to look for

### Speed (throughput, latency, CPU)
- UI: does it repaint when nothing changes? Is the torrent table rebuilt, sorted or filtered every frame instead of when data changes? Are strings formatted per row per frame (`format.rs`)? Does the piece map redraw every frame? Is a large table virtualised?
- Engine adapter: how often are stats polled from librqbit, and does polling take locks the engine also needs? Is any blocking I/O or CPU work (hashing, file moves, `.torrent` parsing, blocklist parsing, regex rules) done on the UI thread or on tokio worker threads instead of `spawn_blocking`?
- Contention: are there locks held across `.await`, coarse global mutexes, or clone-heavy snapshots?
- Feeds, rules, episodes: are regexes compiled once? Is matching a linear scan where an index would do?
- Blocklist: what is the lookup structure (sorted ranges and binary search, or a linear scan)? What does it cost at startup?
- Disk: is the write path efficient? Are config and state saves debounced?
- Startup time: what happens before the first frame appears?

### App size
- Break down the binary by crate. Name the largest contributors (wgpu/naga, egui fonts, aws-lc-rs, reqwest, regex, chrono, librqbit features, accesskit).
- Look for duplicate crate versions and default features you can turn off (tokio, reqwest, librqbit, eframe, rfd).
- Fonts: `default_fonts` and `system_fonts` are both enabled. Is one enough?
- Two TLS or crypto stacks: is `aws-lc-rs` pulled in more than once, or alongside `ring`? Would one provider cover both reqwest and the remote control?
- Profile options to measure, each with a size and speed trade-off: `opt-level = "s"` for cold crates only (through `[profile.release.package.*]`), `panic` strategy (keep unwind; it's a stated choice), `debug = 0`, and `-C target-cpu` for Apple Silicon only builds.
- Bundle: are the icon and assets in `bundle-macos.sh` sized right? Is the zip or dmg compressed well?

### Memory
- Baseline: where do the ~100 MB go? Split it into GPU and Metal buffers, font atlas, tokio runtime, librqbit (piece and peer buffers, DHT table, per-peer state), and app state.
- Unbounded growth: logs (Log window), speed-graph history, feed item history, peer lists, statistics, caches. Each needs a cap.
- Over-retention: full torrent metainfo kept per torrent, cloned `Vec`s per frame, large `String`s that could be `Arc<str>` or interned, a `HashMap` that could be a sorted `Vec`.
- Allocator: what is the "freed pages returned" mechanism in 0.13.3? Would a different allocator (mimalloc) or tuned tokio worker and stack counts help? Measure; don't assume.
- librqbit settings: peer limits, buffer sizes, DHT persistence. Which can the app set, and which need a fork change?

### Architecture quality (only where it affects the four areas)
- `engine.rs` (1.9k lines) and `app.rs` (2.4k lines): are there clear seams to split along that would also make performance work safer?
- Is the core/app boundary clean, with no egui types in core? Can core be tested headless (`examples/headless.rs`)?
- Error handling and panic isolation: is the unwinding choice actually used (`catch_unwind`, task supervision)?
- Test coverage of rules, feeds, episodes and store. Note the gaps that would make the proposed changes risky.

### New features
Start from `docs/designs/qbittorrent-gap.md` and the README feature list. Propose features that fit the product principles (one binary, no web engine, everything in one window). For each one, give:
- the user problem it solves and who asks for it,
- whether librqbit supports it today, supports it with a fork change, or doesn't support it,
- rough effort (S/M/L) and its cost in size and memory,
- dependencies on other recommendations.

Candidates to evaluate (not a mandate): sequential-download toggle (the in-order switch is deferred because it needs a large piece-picker change, so judge that cost), streaming or "play while downloading", per-tracker stats and editing, web seeds (BEP 19), BitTorrent v2 / hybrid torrents, IPv6 and µTP, NAT-PMP alongside UPnP, encryption policy, search plugins, a Spotlight or Quick Look integration, a menu-bar mode, a Shortcuts or AppleScript surface, import from Transmission or qBittorrent, an incomplete-files folder, a disk-space guard, auto-update through Homebrew, and accessibility (VoiceOver labels through accesskit).

## 4. Output

Write the report to `docs/designs/architecture-review.md` (the only file you create), then give a short summary in chat.

The report must contain:

1. **Architecture snapshot.** One page: a diagram (ASCII or Mermaid) of the crates, threads and tasks, and the data flow between engine, store, remote and UI. List the biggest structural risks.
2. **Measurements.** A table of what you measured, the command, and the result, with a column for what you couldn't measure and why.
3. **Findings.** One table per area (Speed, Size, Memory). Each row has:
   - ID (S1, Z1, M1, …), title, evidence (`file:line` and/or a measurement),
   - expected gain (number or range, marked *measured* or *estimated*),
   - effort (S/M/L), risk (Low/Med/High), and "needs fork change" yes or no,
   - the concrete change: what to do, in 2 to 5 sentences, with code sketches only where they clarify.
4. **Features.** A ranked table in the format from section 3.
5. **Roadmap.** Group everything into three releases:
   - **Quick wins:** low risk, one sitting each.
   - **Next release:** medium effort, clear payoff.
   - **Later:** large or needing fork work.
   For each release, say how close it gets to the targets (under 15 MB, under 100 MB).
6. **Things not to do.** Ideas you considered and rejected, with the reason. This includes anything already tried in git history.

Rules for the report:
- Rank by gain divided by effort. Put the three recommendations you'd do first at the very top.
- Every claim needs evidence. Label guesses as guesses.
- Prefer changes in this repo over fork changes, and fork changes over replacing librqbit.
- Keep the stated choices unless you argue against them with numbers: unwinding panics, wgpu over glow, no web engine.
- Plain language, short sentences. No filler.
