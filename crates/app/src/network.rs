//! The Network part of the Settings dialog, and the menu that bans a peer.
//!
//! The Settings dialog shows one line about the network and a button that opens these
//! settings in a window of their own, as they do not fit in it. They are saved with the
//! rest of the settings. Every change is applied by the core starting a new engine session.
use std::path::PathBuf;

use eframe::egui::{self, RichText};
use rusttorrent_core::{Command, Core, NetworkSettings, ProxySettings, Snapshot};

pub struct Form {
    fixed_port: bool,
    port: u16,
    upnp: bool,
    dht: bool,
    local_discovery: bool,
    utp: bool,
    proxy: ProxySettings,
    blocklist: Option<PathBuf>,
    banned: Vec<String>,
    /// The network window is open.
    open: bool,
}

impl Form {
    pub fn of(s: &NetworkSettings) -> Self {
        Self {
            fixed_port: s.port != 0,
            port: if s.port == 0 { 51413 } else { s.port },
            upnp: s.upnp,
            dht: s.dht,
            local_discovery: s.local_discovery,
            utp: s.utp,
            proxy: s.proxy.clone(),
            blocklist: s.blocklist.clone(),
            banned: s.banned.clone(),
            // Testing only: screenshots of the network window.
            #[cfg(feature = "shot")]
            open: std::env::var_os("RUSTTORRENT_SHOT_NETWORK").is_some(),
            #[cfg(not(feature = "shot"))]
            open: false,
        }
    }

    fn settings(&self) -> NetworkSettings {
        NetworkSettings {
            port: if self.fixed_port { self.port } else { 0 },
            upnp: self.upnp,
            dht: self.dht,
            local_discovery: self.local_discovery,
            utp: self.utp,
            proxy: self.proxy.clone(),
            blocklist: self.blocklist.clone(),
            banned: self.banned.clone(),
        }
    }

    /// What stops the settings from being saved. `interface` is the one chosen above.
    pub fn problem(&self, interface: Option<&str>) -> Option<&'static str> {
        if self.proxy.enabled && interface.is_some() {
            return Some(rusttorrent_core::network::BOTH);
        }
        self.proxy.problem()
    }
}

fn note(ui: &mut egui::Ui, text: &str) {
    ui.label(RichText::new(text).small().weak());
}

/// One line about the network in the Settings dialog, and the network window when open.
pub fn section(ui: &mut egui::Ui, form: &mut Form, interface: Option<&str>, remote_on: bool, snap: &Snapshot) {
    if interface.is_some() && remote_on {
        note(
            ui,
            "The remote-control page is not tied to the chosen interface, so a phone on your home \
             network can still open it while torrents use the VPN.",
        );
    }
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        ui.label("Network");
        if ui.button("Change…").clicked() {
            form.open = true;
        }
        ui.add(egui::Label::new(RichText::new(summary(form, snap)).weak()).truncate());
    });
    if let Some(problem) = form.problem(interface) {
        ui.label(RichText::new(problem).small().color(ui.visuals().error_fg_color));
    }
    if form.open {
        let ctx = ui.ctx().clone();
        let tall = (ctx.content_rect().height() - 140.0).max(200.0);
        let response = egui::Modal::new(egui::Id::unique("network")).show(&ctx, |ui| {
            ui.set_width(480.0);
            ui.heading("Network");
            ui.add_space(4.0);
            egui::ScrollArea::vertical()
                .max_height(tall)
                .show(ui, |ui| body(ui, form, snap));
            ui.add_space(4.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button(RichText::new("Done").strong()).clicked() {
                    form.open = false;
                }
                note(ui, "Saved with the other settings.");
            });
        });
        if response.should_close() {
            form.open = false;
        }
    }
}

/// The settings in a few words, as they are in the form.
fn summary(form: &Form, snap: &Snapshot) -> String {
    let mut parts = vec![match (form.fixed_port, snap.network.port) {
        (true, _) => format!("port {}", form.port),
        (false, Some(port)) => format!("port {port} (any)"),
        (false, None) => "any port".to_owned(),
    }];
    if form.proxy.enabled {
        parts.push("proxy on".into());
    } else if form.utp {
        parts.push("uTP on".into());
    }
    if form.blocklist.is_some() {
        parts.push("block list on".into());
    }
    if !form.banned.is_empty() {
        parts.push(format!("{} banned", form.banned.len()));
    }
    let off: Vec<&str> = [
        (form.upnp, "UPnP"),
        (form.dht, "DHT"),
        (form.local_discovery, "local peers"),
    ]
    .into_iter()
    .filter(|(on, _)| !on)
    .map(|(_, name)| name)
    .collect();
    if !off.is_empty() {
        parts.push(format!("{} off", off.join(", ")));
    }
    parts.join(" · ")
}

fn body(ui: &mut egui::Ui, form: &mut Form, snap: &Snapshot) {
    note(
        ui,
        "Changes here restart the torrent engine, so transfers pause for a few seconds.",
    );
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.checkbox(&mut form.fixed_port, "Listen on port");
        ui.add_enabled(
            form.fixed_port,
            egui::DragValue::new(&mut form.port).range(1024..=65535),
        );
        let now = match snap.network.port {
            Some(port) => format!("In use now: {port}"),
            None => "The engine is not running.".to_owned(),
        };
        ui.label(RichText::new(now).small());
    });
    if !form.fixed_port {
        note(ui, "Off: any free port, a new one each time the app starts.");
    }

    ui.add_space(6.0);
    ui.checkbox(&mut form.upnp, "Ask the router to forward the port (UPnP)");
    if !form.upnp {
        note(
            ui,
            "Off: peers on the internet cannot connect to you unless you forward the port on the \
             router yourself, so you find fewer peers.",
        );
    }
    ui.checkbox(&mut form.dht, "Find peers through DHT");
    if !form.dht {
        note(
            ui,
            "Off: magnet links with no trackers cannot start, and you find fewer peers.",
        );
    }
    ui.checkbox(&mut form.local_discovery, "Find peers on my local network");
    if !form.local_discovery {
        note(
            ui,
            "Off: computers on your network sharing the same torrent are not found.",
        );
    }
    ui.add_enabled(
        !form.proxy.enabled,
        egui::Checkbox::new(&mut form.utp, "Also connect over uTP (UDP)"),
    );
    if form.proxy.enabled {
        note(ui, "Not used while the proxy is on: the proxy carries TCP only.");
    } else if form.utp {
        note(
            ui,
            "uTP can reach peers that TCP cannot, and yields to other traffic on a busy line. \
             The torrent engine marks it as new, so turn it off if transfers get worse.",
        );
    }
    note(
        ui,
        "Peer exchange is always on for public torrents: the torrent engine has no switch for it.",
    );

    ui.add_space(6.0);
    let proxy = &mut form.proxy;
    ui.checkbox(&mut proxy.enabled, "Use a SOCKS5 proxy");
    ui.add_enabled_ui(proxy.enabled, |ui| {
        egui::Grid::new("proxy")
            .num_columns(2)
            .spacing([12.0, 4.0])
            .show(ui, |ui| {
                ui.label("Address");
                ui.horizontal(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut proxy.host)
                            .desired_width(180.0)
                            .hint_text("127.0.0.1"),
                    );
                    ui.label("Port");
                    ui.add(egui::DragValue::new(&mut proxy.port).range(0..=65535));
                });
                ui.end_row();
                ui.label("User name");
                ui.add(
                    egui::TextEdit::singleline(&mut proxy.user)
                        .desired_width(180.0)
                        .hint_text("optional"),
                );
                ui.end_row();
                ui.label("Password");
                ui.add(
                    egui::TextEdit::singleline(&mut proxy.password)
                        .password(true)
                        .desired_width(180.0)
                        .hint_text("optional"),
                );
                ui.end_row();
            });
    });
    note(
        ui,
        "Through the proxy: connections to peers, web (HTTP) trackers and feeds. \
         Not through it: UDP trackers, DHT, local peer discovery, port forwarding, and peers \
         that connect to you. To hide all traffic, choose a VPN interface instead.",
    );
    note(
        ui,
        "The proxy password is kept in the settings file on this computer, never in the log.",
    );

    ui.add_space(6.0);
    ui.label("IP block list");
    ui.horizontal(|ui| {
        if ui.button("Choose…").clicked() {
            if let Some(file) = rfd::FileDialog::new().pick_file() {
                form.blocklist = Some(file);
            }
        }
        if form.blocklist.is_some() && ui.button("Remove").clicked() {
            form.blocklist = None;
        }
        let text = match &form.blocklist {
            Some(file) => RichText::new(file.display().to_string()),
            None => RichText::new("No list chosen").weak(),
        };
        ui.add(egui::Label::new(text).truncate());
    });
    note(
        ui,
        "Lines like name:1.2.3.4-1.2.3.255, 1.2.3.0/24 or eMule .dat ranges. Unpack .gz and .zip \
         files first. The file is read again each time the engine starts.",
    );
    let status = &snap.network;
    if snap.network.port.is_some() && status.blocked_ranges > 0 {
        let mut text = format!("Blocking {} address ranges now.", status.blocked_ranges);
        if status.blocklist_bad_lines > 0 {
            text.push_str(&format!(
                " {} lines of the list could not be read.",
                status.blocklist_bad_lines
            ));
        }
        ui.label(RichText::new(text).small());
    }

    ui.add_space(6.0);
    ui.label("Banned peers");
    if form.banned.is_empty() {
        note(ui, "None. Right-click a peer in the Peers tab to ban it.");
    }
    let mut unban = None;
    for (i, ip) in form.banned.iter().enumerate() {
        ui.horizontal(|ui| {
            ui.label(ip);
            if ui.small_button("Unban").clicked() {
                unban = Some(i);
            }
        });
    }
    if let Some(i) = unban {
        form.banned.remove(i);
    }
}

/// Sends the network settings if they changed.
pub fn save(core: &Core, form: &Form, snap: &Snapshot) {
    let settings = form.settings();
    if settings != snap.settings.network {
        core.send(Command::SetNetwork(settings));
    }
}

/// The right-click menu of a peer in the Peers tab.
pub fn peer_menu(ui: &mut egui::Ui, core: &Core, addr: &str) {
    if ui.button("Ban this peer").clicked() {
        core.send(Command::BanPeer(addr.to_owned()));
        ui.close();
    }
}
