//! The queue, the seeding ratio limit and the schedule.
//!
//! Each rule only says whether it holds a torrent back. [`crate::arbiter`] combines the
//! holds with what the user asked for. Nothing here touches the engine or the clock: the
//! caller passes in what it saw, so every rule can be tested with plain values.
use serde::{Deserialize, Serialize};

use crate::arbiter::{Holds, Intent};
use crate::limits::{SeedStop, Seeding, SlowMode, SlowReason};
use crate::store::Settings;

/// When transfers are allowed. Outside the window every torrent is held.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(default)]
pub struct Schedule {
    pub enabled: bool,
    /// Minutes after midnight, local time. Transfers start here.
    pub start_min: u16,
    /// Minutes after midnight. Transfers stop here. Earlier than `start_min` means the
    /// window runs past midnight; equal means the whole day.
    pub end_min: u16,
    /// The days a window may start on. Bit 0 is Monday, bit 6 is Sunday.
    pub days: u8,
}

impl Default for Schedule {
    fn default() -> Self {
        Self {
            enabled: false,
            start_min: 23 * 60,
            end_min: 7 * 60,
            days: 0x7f,
        }
    }
}

/// Local time, as much of it as the schedule needs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Clock {
    /// 0 is Monday, 6 is Sunday.
    pub weekday: u8,
    /// Minutes after midnight.
    pub minute: u16,
}

impl Schedule {
    pub fn allows(&self, now: Clock) -> bool {
        if !self.enabled {
            return true;
        }
        let on = |day: u8| self.days & (1 << (day % 7)) != 0;
        let (start, end, m) = (self.start_min, self.end_min, now.minute);
        if start == end {
            on(now.weekday)
        } else if start < end {
            on(now.weekday) && m >= start && m < end
        } else {
            // Past midnight: tonight's window, or the tail of yesterday's.
            (on(now.weekday) && m >= start) || (on(now.weekday + 6) && m < end)
        }
    }
}

/// What the rules need to know about one torrent.
#[derive(Clone, Debug)]
pub struct Item {
    pub key: String,
    pub intent: Intent,
    /// Everything chosen for download is on disk.
    pub finished: bool,
    /// False while files are being checked or after an error: such a torrent neither
    /// takes a download slot nor waits for one.
    pub settled: bool,
    pub done: u64,
    /// Uploaded over the torrent's whole life.
    pub uploaded: u64,
    pub queue_pos: u64,
    pub added_at: u64,
    /// This torrent's own ratio limit in percent, if it has one. 0 means no limit.
    pub ratio_limit_pct: Option<u32>,
    /// Seeding time so far and this torrent's own seeding time limits.
    pub seeding: Seeding,
    /// Slower than the slow-torrent speeds for a while.
    pub slow: bool,
    /// Never held by the queue and does not use a slot. The schedule still holds it.
    pub force_start: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Outcome {
    pub holds: Holds,
    /// Place in the download queue, from 1. `None` once the torrent is finished.
    pub queue_pos: Option<u32>,
    /// The seeding limit that was reached, if one was. It holds the torrent as a ratio
    /// limit does.
    pub seed_stop: Option<SeedStop>,
}

/// The unfinished torrents in queue order, as indexes into `items`.
fn queue(items: &[Item]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..items.len()).filter(|&i| !items[i].finished).collect();
    order.sort_by(|&a, &b| {
        let (a, b) = (&items[a], &items[b]);
        (a.queue_pos, a.added_at, &a.key).cmp(&(b.queue_pos, b.added_at, &b.key))
    });
    order
}

pub fn ratio_reached(done: u64, uploaded: u64, limit_pct: u32) -> bool {
    limit_pct > 0 && done > 0 && uploaded as u128 * 100 >= done as u128 * limit_pct as u128
}

/// One outcome per item, in the same order.
pub fn apply(items: &[Item], settings: &Settings, now: Clock) -> Vec<Outcome> {
    let schedule = !settings.schedule.allows(now);
    let mut out: Vec<Outcome> = items
        .iter()
        .map(|t| {
            let stop = seed_stop(t, settings);
            Outcome {
                holds: Holds {
                    schedule,
                    ratio: stop.is_some(),
                    ..Default::default()
                },
                queue_pos: None,
                seed_stop: stop,
            }
        })
        .collect();

    let mut free = match settings.max_downloads {
        0 => usize::MAX,
        n => n as usize,
    };
    let limits = &settings.limits;
    let mut active = slots(limits.max_active);
    for (place, i) in queue(items).into_iter().enumerate() {
        out[i].queue_pos = Some(place as u32 + 1);
        let t = &items[i];
        // A paused torrent keeps its place but does not use a slot.
        if t.intent != Intent::Run || !t.settled {
            continue;
        }
        if t.slow && limits.ignore_slow {
            continue;
        }
        // Forced torrents keep their place too, and download beside the queue.
        if t.force_start {
            continue;
        }
        if free > 0 && active > 0 {
            free -= 1;
            active -= 1;
        } else {
            out[i].holds.queue = true;
        }
    }
    // Seeding torrents get what slots are left, in the same order.
    let mut seeding = slots(limits.max_seeding);
    for i in seeds(items) {
        let t = &items[i];
        if t.intent != Intent::Run || !t.settled || out[i].holds.ratio || (t.slow && limits.ignore_slow) {
            continue;
        }
        if seeding > 0 && active > 0 {
            seeding -= 1;
            active -= 1;
        } else {
            out[i].holds.queue = true;
        }
    }
    out
}

fn slots(limit: u32) -> usize {
    match limit {
        0 => usize::MAX,
        n => n as usize,
    }
}

/// The finished torrents in queue order, as indexes into `items`.
fn seeds(items: &[Item]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..items.len()).filter(|&i| items[i].finished).collect();
    order.sort_by(|&a, &b| {
        let (a, b) = (&items[a], &items[b]);
        (a.queue_pos, a.added_at, &a.key).cmp(&(b.queue_pos, b.added_at, &b.key))
    });
    order
}

/// The seeding limit a finished torrent has reached, if any. Its own limits win over the
/// ones in the settings.
pub fn seed_stop(t: &Item, settings: &Settings) -> Option<SeedStop> {
    if !t.finished {
        return None;
    }
    let ratio = t.ratio_limit_pct.unwrap_or(settings.ratio_limit_pct);
    let time = t.seeding.time_limit_min.unwrap_or(settings.limits.seed_time_min);
    let idle = t.seeding.idle_limit_min.unwrap_or(settings.limits.seed_idle_min);
    let past = |secs: u64, min: u32| min > 0 && secs >= min as u64 * 60;
    if ratio_reached(t.done, t.uploaded, ratio) {
        Some(SeedStop::Ratio)
    } else if past(t.seeding.secs, time) {
        Some(SeedStop::Time)
    } else if past(t.seeding.idle_secs, idle) {
        Some(SeedStop::Idle)
    } else {
        None
    }
}

/// Counts `secs` more seconds of seeding. `uploaded` says whether anything went out in
/// that time.
pub fn seed_tick(s: &mut Seeding, secs: u64, uploaded: bool) {
    s.secs = s.secs.saturating_add(secs);
    s.idle_secs = if uploaded { 0 } else { s.idle_secs.saturating_add(secs) };
}

/// Inside slow mode's own scheduled time.
fn slow_window(s: &SlowMode, now: Clock) -> bool {
    s.schedule.enabled && s.schedule.allows(now)
}

/// Whether slow mode is on now, and why.
pub fn slow_mode(s: &SlowMode, now: Clock) -> Option<SlowReason> {
    if s.on {
        Some(SlowReason::Hand)
    } else if slow_window(s, now) && !s.skipped {
        Some(SlowReason::Schedule)
    } else {
        None
    }
}

/// Turns slow mode on or off by hand. Turning it off while its schedule has it on keeps
/// it off until that scheduled time ends.
pub fn set_slow_mode(s: &mut SlowMode, on: bool, now: Clock) {
    s.on = on;
    if !on {
        s.skipped = slow_window(s, now);
    }
}

/// Forgets a skipped scheduled time once it is over. True if anything changed.
pub fn slow_mode_tick(s: &mut SlowMode, now: Clock) -> bool {
    let over = s.skipped && !slow_window(s, now);
    if over {
        s.skipped = false;
    }
    over
}

/// The download and upload limits in KiB/s that apply now (0 means no limit), and why
/// slow mode is on if it is.
pub fn speed_limits(settings: &Settings, now: Clock) -> (u32, u32, Option<SlowReason>) {
    let slow = &settings.limits.slow;
    match slow_mode(slow, now) {
        Some(reason) => (slow.down_kib, slow.up_kib, Some(reason)),
        None => (settings.down_limit_kib, settings.up_limit_kib, None),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Move {
    Top,
    Up,
    Down,
    Bottom,
}

/// Moves the chosen torrents within the queue, keeping their order among themselves.
/// Returns the keys of the unfinished torrents in their new order.
pub fn reorder(items: &[Item], keys: &[String], how: Move) -> Vec<String> {
    let mut order: Vec<&str> = queue(items).into_iter().map(|i| items[i].key.as_str()).collect();
    let chosen = |k: &str| keys.iter().any(|c| c == k);
    match how {
        Move::Top | Move::Bottom => {
            let (mut picked, mut rest): (Vec<&str>, Vec<&str>) = order.iter().partition(|k| chosen(k));
            if how == Move::Top {
                picked.append(&mut rest);
                order = picked;
            } else {
                rest.append(&mut picked);
                order = rest;
            }
        }
        Move::Up => {
            for i in 1..order.len() {
                if chosen(order[i]) && !chosen(order[i - 1]) {
                    order.swap(i, i - 1);
                }
            }
        }
        Move::Down => {
            for i in (0..order.len().saturating_sub(1)).rev() {
                if chosen(order[i]) && !chosen(order[i + 1]) {
                    order.swap(i, i + 1);
                }
            }
        }
    }
    order.into_iter().map(str::to_owned).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(key: &str, pos: u64) -> Item {
        Item {
            key: key.into(),
            intent: Intent::Run,
            finished: false,
            settled: true,
            done: 0,
            uploaded: 0,
            queue_pos: pos,
            added_at: 0,
            ratio_limit_pct: None,
            seeding: Seeding::default(),
            slow: false,
            force_start: false,
        }
    }

    fn at(weekday: u8, hour: u16, minute: u16) -> Clock {
        Clock {
            weekday,
            minute: hour * 60 + minute,
        }
    }

    const NOON: Clock = Clock {
        weekday: 0,
        minute: 720,
    };

    fn queued(items: &[Item], max: u32) -> Vec<bool> {
        let settings = Settings {
            max_downloads: max,
            ..Default::default()
        };
        apply(items, &settings, NOON).iter().map(|o| o.holds.queue).collect()
    }

    #[test]
    fn no_limit_holds_nothing() {
        let items = [item("a", 1), item("b", 2), item("c", 3)];
        assert_eq!(queued(&items, 0), [false, false, false]);
    }

    #[test]
    fn only_the_first_n_download() {
        // Given out of order on purpose: the queue position decides, not the list order.
        let items = [item("c", 3), item("a", 1), item("b", 2)];
        assert_eq!(queued(&items, 2), [true, false, false]);
        let out = apply(
            &items,
            &Settings {
                max_downloads: 2,
                ..Default::default()
            },
            NOON,
        );
        assert_eq!(
            out.iter().map(|o| o.queue_pos).collect::<Vec<_>>(),
            [Some(3), Some(1), Some(2)]
        );
    }

    #[test]
    fn finished_and_paused_torrents_use_no_slot() {
        let mut seeding = item("a", 1);
        seeding.finished = true;
        let mut paused = item("b", 2);
        paused.intent = Intent::Pause;
        let mut checking = item("c", 3);
        checking.settled = false;
        let items = [seeding, paused, checking, item("d", 4), item("e", 5)];
        assert_eq!(queued(&items, 1), [false, false, false, false, true]);
    }

    #[test]
    fn a_slot_frees_when_a_download_finishes() {
        let mut items = [item("a", 1), item("b", 2)];
        assert_eq!(queued(&items, 1), [false, true]);
        items[0].finished = true;
        assert_eq!(queued(&items, 1), [false, false]);
    }

    #[test]
    fn torrents_from_before_the_queue_existed_go_by_age() {
        let mut old = item("z", 0);
        old.added_at = 10;
        let mut older = item("y", 0);
        older.added_at = 5;
        assert_eq!(queued(&[old, older, item("new", 1)], 1), [true, false, true]);
    }

    #[test]
    fn ratio_limit() {
        assert!(!ratio_reached(1000, 5000, 0), "0 means no limit");
        assert!(!ratio_reached(1000, 1499, 150));
        assert!(ratio_reached(1000, 1500, 150));
        assert!(!ratio_reached(0, 0, 100), "nothing downloaded yet");
        assert!(ratio_reached(u64::MAX, u64::MAX, 100), "no overflow");
    }

    #[test]
    fn ratio_holds_only_finished_torrents_and_own_limit_wins() {
        let settings = Settings {
            ratio_limit_pct: 100,
            ..Default::default()
        };
        let mut t = item("a", 1);
        t.done = 1000;
        t.uploaded = 2000;
        let ratio = |t: &Item| apply(std::slice::from_ref(t), &settings, NOON)[0].holds.ratio;
        assert!(!ratio(&t), "still downloading");
        t.finished = true;
        assert!(ratio(&t));
        t.ratio_limit_pct = Some(0);
        assert!(!ratio(&t), "this torrent has no limit");
        t.ratio_limit_pct = Some(300);
        assert!(!ratio(&t));
        t.uploaded = 3000;
        assert!(ratio(&t));
    }

    #[test]
    fn schedule_off_allows_everything() {
        assert!(Schedule::default().allows(at(3, 12, 0)));
    }

    #[test]
    fn schedule_within_one_day() {
        let s = Schedule {
            enabled: true,
            start_min: 9 * 60,
            end_min: 17 * 60,
            days: 0b001_1111, // Monday to Friday
        };
        assert!(!s.allows(at(0, 8, 59)));
        assert!(s.allows(at(0, 9, 0)));
        assert!(s.allows(at(4, 16, 59)));
        assert!(!s.allows(at(4, 17, 0)));
        assert!(!s.allows(at(5, 12, 0)), "Saturday");
    }

    #[test]
    fn schedule_past_midnight() {
        let s = Schedule {
            enabled: true,
            start_min: 23 * 60,
            end_min: 7 * 60,
            days: 0b000_0001, // Monday night only
        };
        assert!(!s.allows(at(0, 22, 59)));
        assert!(s.allows(at(0, 23, 0)));
        assert!(s.allows(at(1, 6, 59)), "Tuesday morning belongs to Monday night");
        assert!(!s.allows(at(1, 7, 0)));
        assert!(!s.allows(at(0, 3, 0)), "Monday morning would belong to Sunday night");
        assert!(!s.allows(at(1, 23, 30)));
    }

    #[test]
    fn schedule_whole_day_and_sunday_wraps_to_monday() {
        let s = Schedule {
            enabled: true,
            start_min: 0,
            end_min: 0,
            days: 0b100_0000, // Sunday
        };
        assert!(s.allows(at(6, 0, 0)));
        assert!(s.allows(at(6, 23, 59)));
        assert!(!s.allows(at(0, 0, 0)));
        let s = Schedule {
            start_min: 22 * 60,
            end_min: 60,
            ..s
        };
        assert!(s.allows(at(0, 0, 30)), "Monday 00:30 is the tail of Sunday night");
    }

    #[test]
    fn schedule_holds_every_torrent() {
        let settings = Settings {
            schedule: Schedule {
                enabled: true,
                start_min: 60,
                end_min: 120,
                days: 0x7f,
            },
            ..Default::default()
        };
        let mut done = item("b", 2);
        done.finished = true;
        let out = apply(&[item("a", 1), done], &settings, NOON);
        assert!(out.iter().all(|o| o.holds.schedule));
        let out = apply(&[item("a", 1)], &settings, at(0, 1, 30));
        assert!(!out[0].holds.schedule);
    }

    fn keys(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn moving_in_the_queue() {
        let items = [item("a", 1), item("b", 2), item("c", 3), item("d", 4)];
        let mv = |chosen: &[&str], how| reorder(&items, &keys(chosen), how);
        assert_eq!(mv(&["c"], Move::Up), keys(&["a", "c", "b", "d"]));
        assert_eq!(mv(&["a"], Move::Up), keys(&["a", "b", "c", "d"]));
        assert_eq!(mv(&["b"], Move::Down), keys(&["a", "c", "b", "d"]));
        assert_eq!(mv(&["d"], Move::Down), keys(&["a", "b", "c", "d"]));
        assert_eq!(mv(&["c", "d"], Move::Top), keys(&["c", "d", "a", "b"]));
        assert_eq!(mv(&["a", "c"], Move::Bottom), keys(&["b", "d", "a", "c"]));
        // A block moves together and stops at the edge.
        assert_eq!(mv(&["b", "c"], Move::Up), keys(&["b", "c", "a", "d"]));
        assert_eq!(mv(&["a", "b"], Move::Up), keys(&["a", "b", "c", "d"]));
        assert_eq!(mv(&["b", "c"], Move::Down), keys(&["a", "d", "b", "c"]));
        assert_eq!(mv(&["c", "d"], Move::Down), keys(&["a", "b", "c", "d"]));
    }

    #[test]
    fn finished_torrents_are_not_in_the_queue() {
        let mut done = item("b", 2);
        done.finished = true;
        let items = [item("a", 1), done, item("c", 3)];
        assert_eq!(reorder(&items, &keys(&["c"]), Move::Up), keys(&["c", "a"]));
    }

    fn limited(limits: crate::limits::LimitSettings) -> Settings {
        Settings {
            limits,
            ..Default::default()
        }
    }

    fn seed(key: &str, pos: u64) -> Item {
        let mut t = item(key, pos);
        t.finished = true;
        t.done = 1000;
        t
    }

    #[test]
    fn slow_mode_by_hand_and_by_schedule() {
        let mut s = SlowMode {
            schedule: Schedule {
                enabled: true,
                start_min: 9 * 60,
                end_min: 17 * 60,
                days: 0x7f,
            },
            ..Default::default()
        };
        assert_eq!(slow_mode(&s, at(0, 8, 0)), None);
        assert_eq!(slow_mode(&s, NOON), Some(SlowReason::Schedule));
        s.on = true;
        assert_eq!(slow_mode(&s, at(0, 8, 0)), Some(SlowReason::Hand));
        assert_eq!(slow_mode(&s, NOON), Some(SlowReason::Hand), "by hand is named first");
        s.schedule.enabled = false;
        s.on = false;
        assert_eq!(slow_mode(&s, NOON), None, "a schedule that is off never turns it on");
    }

    #[test]
    fn turning_slow_mode_off_skips_the_rest_of_its_scheduled_time() {
        let mut s = SlowMode {
            schedule: Schedule {
                enabled: true,
                start_min: 9 * 60,
                end_min: 17 * 60,
                days: 0x7f,
            },
            ..Default::default()
        };
        set_slow_mode(&mut s, false, NOON);
        assert_eq!(slow_mode(&s, NOON), None);
        assert!(!slow_mode_tick(&mut s, at(0, 16, 59)), "still inside the time");
        assert_eq!(slow_mode(&s, at(0, 16, 59)), None);
        assert!(slow_mode_tick(&mut s, at(0, 17, 0)), "the time is over");
        assert_eq!(
            slow_mode(&s, at(1, 9, 0)),
            Some(SlowReason::Schedule),
            "back the next day"
        );
        // Off by hand outside the scheduled time skips nothing.
        set_slow_mode(&mut s, true, at(0, 20, 0));
        set_slow_mode(&mut s, false, at(0, 20, 0));
        assert!(!s.skipped);
        assert_eq!(slow_mode(&s, at(1, 9, 0)), Some(SlowReason::Schedule));
        // On by hand inside a skipped time turns it on.
        set_slow_mode(&mut s, false, NOON);
        set_slow_mode(&mut s, true, NOON);
        assert_eq!(slow_mode(&s, NOON), Some(SlowReason::Hand));
    }

    #[test]
    fn speed_limits_follow_slow_mode() {
        let mut settings = Settings {
            down_limit_kib: 5000,
            up_limit_kib: 0,
            ..Default::default()
        };
        settings.limits.slow.down_kib = 100;
        settings.limits.slow.up_kib = 10;
        assert_eq!(speed_limits(&settings, NOON), (5000, 0, None));
        settings.limits.slow.on = true;
        assert_eq!(speed_limits(&settings, NOON), (100, 10, Some(SlowReason::Hand)));
    }

    #[test]
    fn slow_mode_and_the_all_or_nothing_schedule_work_side_by_side() {
        let mut settings = Settings {
            schedule: Schedule {
                enabled: true,
                start_min: 0,
                end_min: 6 * 60,
                days: 0x7f,
            },
            ..Default::default()
        };
        settings.limits.slow.schedule = Schedule {
            enabled: true,
            start_min: 0,
            end_min: 0,
            days: 0x7f,
        };
        let out = apply(&[item("a", 1)], &settings, NOON);
        assert!(out[0].holds.schedule, "transfers are off at noon");
        assert_eq!(speed_limits(&settings, NOON).2, Some(SlowReason::Schedule));
        let out = apply(&[item("a", 1)], &settings, at(0, 3, 0));
        assert!(!out[0].holds.schedule);
    }

    #[test]
    fn seeding_time_limits() {
        let mut settings = limited(crate::limits::LimitSettings {
            seed_time_min: 60,
            seed_idle_min: 10,
            ..Default::default()
        });
        let mut t = seed("a", 1);
        assert_eq!(seed_stop(&t, &settings), None);
        t.seeding.secs = 3600;
        assert_eq!(seed_stop(&t, &settings), Some(SeedStop::Time));
        t.seeding.secs = 0;
        t.seeding.idle_secs = 600;
        assert_eq!(seed_stop(&t, &settings), Some(SeedStop::Idle));
        t.seeding.idle_limit_min = Some(0);
        assert_eq!(seed_stop(&t, &settings), None, "this torrent has no idle limit");
        t.seeding.time_limit_min = Some(5);
        t.seeding.secs = 300;
        assert_eq!(seed_stop(&t, &settings), Some(SeedStop::Time), "own limit wins");
        settings.ratio_limit_pct = 100;
        t.uploaded = 1000;
        assert_eq!(seed_stop(&t, &settings), Some(SeedStop::Ratio), "ratio is named first");
        let out = apply(std::slice::from_ref(&t), &settings, NOON);
        assert!(out[0].holds.ratio);
        assert_eq!(out[0].seed_stop, Some(SeedStop::Ratio));
        t.finished = false;
        assert_eq!(seed_stop(&t, &settings), None, "still downloading");
    }

    #[test]
    fn seed_tick_counts_time_and_idle_time() {
        let mut s = Seeding::default();
        seed_tick(&mut s, 5, false);
        seed_tick(&mut s, 3, false);
        assert_eq!((s.secs, s.idle_secs), (8, 8));
        seed_tick(&mut s, 2, true);
        assert_eq!((s.secs, s.idle_secs), (10, 0));
        seed_tick(&mut s, 0, false);
        assert_eq!((s.secs, s.idle_secs), (10, 0));
    }

    fn holds(items: &[Item], limits: crate::limits::LimitSettings, max_downloads: u32) -> Vec<bool> {
        let settings = Settings {
            max_downloads,
            limits,
            ..Default::default()
        };
        apply(items, &settings, NOON).iter().map(|o| o.holds.queue).collect()
    }

    #[test]
    fn only_so_many_seed_at_once() {
        let limits = crate::limits::LimitSettings {
            max_seeding: 2,
            ..Default::default()
        };
        let mut paused = seed("b", 2);
        paused.intent = Intent::Pause;
        let items = [seed("c", 4), seed("a", 1), paused, seed("d", 3), item("x", 5)];
        assert_eq!(holds(&items, limits, 0), [true, false, false, false, false]);
    }

    #[test]
    fn a_torrent_stopped_by_a_seeding_limit_frees_its_slot() {
        let mut settings = limited(crate::limits::LimitSettings {
            max_seeding: 1,
            ..Default::default()
        });
        settings.ratio_limit_pct = 100;
        let mut done = seed("a", 1);
        done.uploaded = 1000;
        let out = apply(&[done, seed("b", 2)], &settings, NOON);
        assert!(out[0].holds.ratio && !out[0].holds.queue);
        assert!(!out[1].holds.queue);
    }

    #[test]
    fn active_limit_counts_downloads_first_then_seeds() {
        let limits = crate::limits::LimitSettings {
            max_active: 3,
            ..Default::default()
        };
        let items = [seed("s1", 1), item("d1", 2), item("d2", 3), seed("s2", 4)];
        assert_eq!(holds(&items, limits, 0), [false, false, false, true]);
        // The download limit still applies inside the active limit.
        assert_eq!(holds(&items, limits, 1), [false, false, true, false]);
    }

    #[test]
    fn slow_torrents_can_be_left_out_of_the_limits() {
        let mut limits = crate::limits::LimitSettings {
            max_active: 1,
            ..Default::default()
        };
        let mut slow = item("a", 1);
        slow.slow = true;
        let mut slow_seed = seed("s", 1);
        slow_seed.slow = true;
        let items = [slow, item("b", 2), slow_seed, seed("t", 2)];
        assert_eq!(holds(&items, limits, 0), [false, true, true, true]);
        limits.ignore_slow = true;
        assert_eq!(holds(&items, limits, 0), [false, false, false, true]);
    }

    #[test]
    fn force_start_skips_the_queue_and_uses_no_slot() {
        let mut forced = item("c", 3);
        forced.force_start = true;
        let items = [item("a", 1), item("b", 2), forced, item("d", 4)];
        // One slot: "a" takes it, "c" downloads anyway, "b" and "d" wait.
        assert_eq!(queued(&items, 1), [false, true, false, true]);
        let out = apply(
            &items,
            &Settings {
                max_downloads: 1,
                ..Default::default()
            },
            NOON,
        );
        assert_eq!(out[2].queue_pos, Some(3), "it keeps its place in the queue");
        let decisions: Vec<_> = items
            .iter()
            .zip(&out)
            .map(|(t, o)| crate::arbiter::decide(t.intent, o.holds))
            .collect();
        assert_eq!(decisions[2], crate::arbiter::Decision::Run);
    }

    #[test]
    fn force_start_still_obeys_the_schedule_and_pause() {
        let settings = Settings {
            max_downloads: 1,
            schedule: Schedule {
                enabled: true,
                start_min: 60,
                end_min: 120,
                days: 0x7f,
            },
            ..Default::default()
        };
        let mut forced = item("a", 1);
        forced.force_start = true;
        let out = apply(std::slice::from_ref(&forced), &settings, NOON);
        assert!(out[0].holds.schedule, "outside the window it is held");
        assert!(!out[0].holds.queue);
        assert_ne!(
            crate::arbiter::decide(forced.intent, out[0].holds),
            crate::arbiter::Decision::Run
        );
        forced.intent = Intent::Pause;
        let out = apply(std::slice::from_ref(&forced), &settings, at(0, 1, 30));
        assert_eq!(
            crate::arbiter::decide(forced.intent, out[0].holds),
            crate::arbiter::Decision::Paused,
            "a paused torrent stays paused"
        );
    }

    #[test]
    fn force_start_still_obeys_the_network_block() {
        let mut forced = item("a", 1);
        forced.force_start = true;
        let mut holds = apply(std::slice::from_ref(&forced), &Settings::default(), NOON)[0].holds;
        holds.network = true;
        assert_eq!(
            crate::arbiter::decide(forced.intent, holds),
            crate::arbiter::Decision::Held(crate::arbiter::HoldReason::Network)
        );
    }
}
