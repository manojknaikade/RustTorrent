//! The queue, the seeding ratio limit and the schedule.
//!
//! Each rule only says whether it holds a torrent back. [`crate::arbiter`] combines the
//! holds with what the user asked for. Nothing here touches the engine or the clock: the
//! caller passes in what it saw, so every rule can be tested with plain values.
use serde::{Deserialize, Serialize};

use crate::arbiter::{Holds, Intent};
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
    /// Never held by the queue and does not use a slot. The schedule still holds it.
    pub force_start: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Outcome {
    pub holds: Holds,
    /// Place in the download queue, from 1. `None` once the torrent is finished.
    pub queue_pos: Option<u32>,
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
            let limit = t.ratio_limit_pct.unwrap_or(settings.ratio_limit_pct);
            Outcome {
                holds: Holds {
                    schedule,
                    ratio: t.finished && ratio_reached(t.done, t.uploaded, limit),
                    ..Default::default()
                },
                queue_pos: None,
            }
        })
        .collect();

    let mut free = match settings.max_downloads {
        0 => usize::MAX,
        n => n as usize,
    };
    for (place, i) in queue(items).into_iter().enumerate() {
        out[i].queue_pos = Some(place as u32 + 1);
        let t = &items[i];
        // A paused torrent keeps its place but does not use a slot.
        if t.intent != Intent::Run || !t.settled {
            continue;
        }
        // Forced torrents keep their place too, and download beside the queue.
        if t.force_start {
            continue;
        }
        if free > 0 {
            free -= 1;
        } else {
            out[i].holds.queue = true;
        }
    }
    out
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
