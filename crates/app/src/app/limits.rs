//! Slow mode, seeding time limits, queue limits and peers per torrent: the switch, the
//! window that sets them, and what the main window shows about them.
use eframe::egui::{self, RichText};
use rusttorrent_core::limits::ENGINE_PEERS;
use rusttorrent_core::{format, Command, Core, LimitSettings, SeedStop, SlowReason, Snapshot, State, TorrentView};

use super::{clock_text, color, time_edit, DAYS};

pub(super) struct LimitsDialog {
    limits: LimitSettings,
}

impl LimitsDialog {
    pub(super) fn of(snap: &Snapshot) -> Self {
        Self {
            limits: snap.settings.limits,
        }
    }

    /// Draws the window. Returns true once it is closed.
    pub(super) fn show(&mut self, ctx: &egui::Context, core: &Core) -> bool {
        let (mut close, mut save) = (false, false);
        let l = &mut self.limits;
        let response = egui::Modal::new(egui::Id::unique("limits")).show(ctx, |ui| {
            ui.set_width(460.0);
            ui.heading("Limits");
            ui.add_space(6.0);
            ui.label(RichText::new("Slow mode: speed limits in KiB/s. 0 means no limit.").weak());
            egui::Grid::new("slow")
                .num_columns(2)
                .spacing([12.0, 6.0])
                .show(ui, |ui| {
                    ui.label("Download");
                    ui.add(
                        egui::DragValue::new(&mut l.slow.down_kib)
                            .range(0..=4_000_000)
                            .speed(10),
                    );
                    ui.end_row();
                    ui.label("Upload");
                    ui.add(egui::DragValue::new(&mut l.slow.up_kib).range(0..=4_000_000).speed(10));
                    ui.end_row();
                });
            let sch = &mut l.slow.schedule;
            ui.checkbox(&mut sch.enabled, "Turn slow mode on at these times");
            ui.add_enabled_ui(sch.enabled, |ui| {
                ui.horizontal(|ui| {
                    ui.label("From");
                    time_edit(ui, &mut sch.start_min);
                    ui.label("to");
                    time_edit(ui, &mut sch.end_min);
                    if sch.start_min > sch.end_min {
                        ui.label(RichText::new("the next day").weak());
                    } else if sch.start_min == sch.end_min {
                        ui.label(RichText::new("the whole day").weak());
                    }
                });
                ui.horizontal(|ui| {
                    ui.label("Starting on");
                    for (i, day) in DAYS.iter().enumerate() {
                        let bit = 1u8 << i;
                        if ui.selectable_label(sch.days & bit != 0, *day).clicked() {
                            sch.days ^= bit;
                        }
                    }
                });
            });
            ui.label(
                RichText::new("Slow mode can also be turned on and off by hand at the bottom of the window.")
                    .small()
                    .weak(),
            );

            ui.add_space(8.0);
            ui.label(RichText::new("Seeding, in minutes. 0 means no limit.").weak());
            egui::Grid::new("seeding")
                .num_columns(2)
                .spacing([12.0, 6.0])
                .show(ui, |ui| {
                    ui.label("Stop seeding after");
                    ui.add(egui::DragValue::new(&mut l.seed_time_min).range(0..=525_600).speed(1));
                    ui.end_row();
                    ui.label("Stop after nothing uploaded for");
                    ui.add(egui::DragValue::new(&mut l.seed_idle_min).range(0..=525_600).speed(1));
                    ui.end_row();
                });

            ui.add_space(8.0);
            ui.label(RichText::new("Queue. 0 means no limit.").weak());
            egui::Grid::new("queue")
                .num_columns(2)
                .spacing([12.0, 6.0])
                .show(ui, |ui| {
                    ui.label("Seeding at once");
                    ui.add(egui::DragValue::new(&mut l.max_seeding).range(0..=1000).speed(0.1));
                    ui.end_row();
                    ui.label("Downloading and seeding at once");
                    ui.add(egui::DragValue::new(&mut l.max_active).range(0..=1000).speed(0.1));
                    ui.end_row();
                });
            ui.checkbox(&mut l.ignore_slow, "Do not count slow torrents");
            ui.add_enabled_ui(l.ignore_slow, |ui| {
                ui.horizontal(|ui| {
                    ui.label("Slow means below ↓");
                    ui.add(egui::DragValue::new(&mut l.slow_down_kib).range(0..=100_000));
                    ui.label("and ↑");
                    ui.add(egui::DragValue::new(&mut l.slow_up_kib).range(0..=100_000));
                    ui.label("KiB/s for a minute");
                });
            });

            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.label("Peers per torrent");
                ui.add(egui::DragValue::new(&mut l.peers_per_torrent).range(0..=2000).speed(1));
                ui.label(RichText::new(format!("0 means {ENGINE_PEERS}")).weak());
            });
            ui.label(
                RichText::new("Changing this stops and starts all transfers once.")
                    .small()
                    .weak(),
            );

            ui.add_space(4.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button(RichText::new("Save").strong()).clicked() {
                    save = true;
                }
                if ui.button("Cancel").clicked() {
                    close = true;
                }
            });
        });
        if save {
            core.send(Command::SetLimitSettings(self.limits));
            close = true;
        }
        close || response.should_close()
    }
}

fn kib_text(kib: u32) -> String {
    if kib == 0 {
        "no limit".to_owned()
    } else {
        format!("{kib} KiB/s")
    }
}

/// The speed limits that apply now, in KiB/s.
pub(super) fn speed_limits(snap: &Snapshot) -> (u32, u32) {
    let s = &snap.settings;
    match snap.slow {
        Some(_) => (s.limits.slow.down_kib, s.limits.slow.up_kib),
        None => (s.down_limit_kib, s.up_limit_kib),
    }
}

/// The slow mode switch and, while it is on, why. For the status bar, right to left.
/// Returns true when the limits text should open the limits window.
pub(super) fn status(ui: &mut egui::Ui, core: &Core, snap: &Snapshot) -> bool {
    let (down, up) = speed_limits(snap);
    let text = format!("Limits: ↓ {}  ↑ {}", kib_text(down), kib_text(up));
    let open = ui
        .add(egui::Button::new(RichText::new(text).small()).frame(false))
        .on_hover_text("Change slow mode, seeding and queue limits")
        .clicked();
    let slow = &snap.settings.limits.slow;
    let on = snap.slow.is_some();
    let switch = ui
        .selectable_label(on, RichText::new("Slow mode").small())
        .on_hover_text(format!(
            "Limit speeds to ↓ {}  ↑ {}",
            kib_text(slow.down_kib),
            kib_text(slow.up_kib)
        ));
    if switch.clicked() {
        core.send(Command::SetSlowMode(!on));
    }
    let why = match snap.slow {
        Some(SlowReason::Hand) => Some("Slow mode is on".to_owned()),
        Some(SlowReason::Schedule) => Some(format!(
            "Slow mode is on by schedule until {}",
            clock_text(slow.schedule.end_min)
        )),
        None => None,
    };
    if let Some(why) = why {
        ui.label(RichText::new(why).small().color(color(State::Scheduled)));
    }
    open
}

/// Minutes as "45 min", "2h 0m", or "none" for 0.
fn minutes_text(min: u32) -> String {
    match min {
        0 => "none".to_owned(),
        m if m < 60 => format!("{m} min"),
        m => format::duration(m as u64 * 60),
    }
}

/// Rows for the overview: seeding time, its limits and whether the torrent counts as slow.
pub(super) fn overview_row(t: &TorrentView, snap: &Snapshot) -> [(&'static str, String); 3] {
    let s = &t.limits.seeding;
    let l = &snap.settings.limits;
    let time = s.time_limit_min.unwrap_or(l.seed_time_min);
    let idle = s.idle_limit_min.unwrap_or(l.seed_idle_min);
    let own = if s.time_limit_min.is_some() || s.idle_limit_min.is_some() {
        " (this torrent)"
    } else {
        ""
    };
    let limits = match (time, idle) {
        (0, 0) => "none".to_owned(),
        (t, 0) => minutes_text(t),
        (0, i) => format!("{} idle", minutes_text(i)),
        (t, i) => format!("{}, {} idle", minutes_text(t), minutes_text(i)),
    };
    [
        ("Seeding time", format::duration(s.secs)),
        ("Seeding limit", format!("{limits}{own}")),
        ("Slow", if t.limits.slow { "Yes" } else { "No" }.to_owned()),
    ]
}

/// What a torrent stopped by a seeding limit waits for.
pub(super) fn stopped_text(t: &TorrentView) -> &'static str {
    match t.limits.stop {
        Some(SeedStop::Time) => "Nothing: seeding time limit reached",
        Some(SeedStop::Idle) => "Nothing: nothing uploaded for too long",
        _ => "Nothing: ratio limit reached",
    }
}

/// The right-click menu for one torrent's own seeding time limits.
pub(super) fn seeding_menu(ui: &mut egui::Ui, t: &TorrentView, keys: &[String]) -> Option<Command> {
    let s = t.limits.seeding;
    let mut chosen = None;
    let options = [
        ("Use the setting", None),
        ("No limit", Some(0)),
        ("1 hour", Some(60)),
        ("1 day", Some(24 * 60)),
        ("1 week", Some(7 * 24 * 60)),
    ];
    ui.menu_button("Seeding time limit", |ui| {
        for (name, limit) in options {
            if ui.radio(s.time_limit_min == limit, name).clicked() {
                chosen = Some((limit, s.idle_limit_min));
                ui.close();
            }
        }
    });
    let options = [
        ("Use the setting", None),
        ("No limit", Some(0)),
        ("30 minutes", Some(30)),
        ("2 hours", Some(120)),
        ("1 day", Some(24 * 60)),
    ];
    ui.menu_button("Stop when nothing uploaded for", |ui| {
        for (name, limit) in options {
            if ui.radio(s.idle_limit_min == limit, name).clicked() {
                chosen = Some((s.time_limit_min, limit));
                ui.close();
            }
        }
    });
    chosen.map(|(time_limit_min, idle_limit_min)| Command::SetSeedingLimits {
        keys: keys.to_vec(),
        time_limit_min,
        idle_limit_min,
    })
}
