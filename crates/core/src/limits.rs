//! Slow mode, seeding time limits and queue limits: the settings and the counters kept
//! for them. The rules that read them are in [`crate::rules`].
use serde::{Deserialize, Serialize};

use crate::rules::Schedule;

/// A second set of speed limits, turned on by hand or by its own schedule.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(default)]
pub struct SlowMode {
    /// Turned on by hand.
    pub on: bool,
    /// Limits in KiB/s while slow mode is on. 0 means no limit.
    pub down_kib: u32,
    pub up_kib: u32,
    /// When slow mode turns itself on. Off unless `enabled`.
    pub schedule: Schedule,
    /// Turned off by hand while its schedule had it on. Lasts until that time ends.
    pub skipped: bool,
}

impl Default for SlowMode {
    fn default() -> Self {
        Self {
            on: false,
            down_kib: 1024,
            up_kib: 128,
            schedule: Schedule {
                enabled: false,
                start_min: 9 * 60,
                end_min: 17 * 60,
                days: 0b001_1111,
            },
            skipped: false,
        }
    }
}

/// Why slow mode is on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SlowReason {
    Hand,
    Schedule,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(default)]
pub struct LimitSettings {
    pub slow: SlowMode,
    /// Stop seeding after this many minutes of seeding. 0 means no limit.
    pub seed_time_min: u32,
    /// Stop seeding after this many minutes with nothing uploaded. 0 means no limit.
    pub seed_idle_min: u32,
    /// How many torrents may seed at once. 0 means no limit.
    pub max_seeding: u32,
    /// How many torrents may download or seed at once, together. 0 means no limit.
    pub max_active: u32,
    /// Torrents slower than the speeds below do not count against the limits above.
    pub ignore_slow: bool,
    /// In KiB/s. A torrent below both for a minute counts as slow.
    pub slow_down_kib: u32,
    pub slow_up_kib: u32,
    /// Peers each torrent may be connected to at once. 0 uses the engine's own number.
    pub peers_per_torrent: u32,
}

impl Default for LimitSettings {
    fn default() -> Self {
        Self {
            slow: SlowMode::default(),
            seed_time_min: 0,
            seed_idle_min: 0,
            max_seeding: 0,
            max_active: 0,
            ignore_slow: false,
            slow_down_kib: 2,
            slow_up_kib: 2,
            peers_per_torrent: 0,
        }
    }
}

/// The engine's own number of peers per torrent, used when the setting is 0.
pub const ENGINE_PEERS: u32 = 128;

/// One torrent's seeding time limits and how long it has seeded.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[serde(default)]
pub struct Seeding {
    /// This torrent's own limits in minutes, instead of the ones in the settings.
    /// 0 means no limit.
    pub time_limit_min: Option<u32>,
    pub idle_limit_min: Option<u32>,
    /// Seconds spent seeding, across restarts.
    pub secs: u64,
    /// Seconds spent seeding since anything was last uploaded.
    pub idle_secs: u64,
}

/// Which limit stopped a torrent from seeding.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SeedStop {
    Ratio,
    Time,
    Idle,
}

/// What the window shows about one torrent's limits.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TorrentLimits {
    pub seeding: Seeding,
    /// Set once a seeding limit is reached.
    pub stop: Option<SeedStop>,
    /// Slower than the slow-torrent speeds for a minute or more.
    pub slow: bool,
}
