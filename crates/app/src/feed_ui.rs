//! The newer parts of the Feeds dialog: the rule options under each feed, and a preview
//! of what a feed lists with what its rule would do with each item.
use eframe::egui::{self, RichText};
use rusttorrent_core::feeds::{Item, Verdict};
use rusttorrent_core::{Command, Core, Feed, FeedPreview};

/// The checkboxes under a feed's words.
pub(crate) fn options(ui: &mut egui::Ui, feed: &mut Feed) {
    ui.horizontal_wrapped(|ui| {
        ui.checkbox(&mut feed.regex, "Words are regular expressions")
            .on_hover_text("“Must have” and “Must not have” are each one expression, matched in any case.");
        ui.checkbox(&mut feed.skip_repeats, "Skip episodes already downloaded")
            .on_hover_text("The same season and episode is downloaded once, unless the title says REPACK or PROPER.");
        ui.checkbox(&mut feed.newer_only, "Only items newer than the feed")
            .on_hover_text("What the feed lists when it is first read is skipped.");
    });
}

/// What is wrong with the feed's rule, shown in red. `true` when something is.
pub(crate) fn problem(ui: &mut egui::Ui, feed: &Feed) -> bool {
    match feed.rule() {
        Ok(_) => false,
        Err(e) => {
            ui.label(RichText::new(e).small().color(ui.visuals().error_fg_color));
            true
        }
    }
}

fn verdict_text(verdict: Verdict) -> &'static str {
    match verdict {
        Verdict::Take => "Matches",
        Verdict::NoMatch => "No match",
        Verdict::Seen => "Downloaded before",
        Verdict::Repeat => "Episode downloaded before",
    }
}

/// The preview of one feed: its items, newest first, and what the rule makes of each.
/// `feed` is the feed as it is in the dialog, saved or not. Returns `true` when the user
/// closes it.
pub(crate) fn preview(ui: &mut egui::Ui, core: &Core, preview: &FeedPreview, feed: Option<&Feed>) -> bool {
    let mut close = false;
    ui.group(|ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            let title = feed
                .map(|f| f.name.trim())
                .filter(|n| !n.is_empty())
                .unwrap_or(&preview.url);
            ui.add(egui::Label::new(RichText::new(format!("Preview: {title}")).strong()).truncate());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Close preview").clicked() {
                    close = true;
                }
            });
        });
        if preview.loading {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Reading the feed…");
            });
            return;
        }
        if let Some(error) = &preview.error {
            ui.label(RichText::new(error).color(ui.visuals().error_fg_color));
            return;
        }
        let rule = feed.map(Feed::rule);
        if feed.is_some_and(|f| f.newer_only) && !preview.primed {
            ui.label(
                RichText::new("Only newer items are taken: these are all skipped when the feed is first read.")
                    .small()
                    .weak(),
            );
        }
        let category = feed.and_then(|f| f.category.clone());
        egui::ScrollArea::vertical()
            .id_salt("feed preview")
            .max_height(220.0)
            .show(ui, |ui| {
                egui::Grid::new("feed preview items")
                    .num_columns(3)
                    .spacing([10.0, 4.0])
                    .striped(true)
                    .show(ui, |ui| {
                        for item in &preview.items {
                            let verdict = match &rule {
                                Some(Ok(rule)) => Some(rule.judge(item, &preview.seen, &preview.taken)),
                                _ => None,
                            };
                            let text = verdict.map_or("", verdict_text);
                            let label = if verdict == Some(Verdict::Take) {
                                RichText::new(text).strong()
                            } else {
                                RichText::new(text).weak()
                            };
                            ui.label(label.small());
                            ui.add(egui::Label::new(&item.title).truncate());
                            if ui.small_button("Download").clicked() {
                                download(core, preview, item, category.clone());
                            }
                            ui.end_row();
                        }
                    });
            });
    });
    close
}

fn download(core: &Core, preview: &FeedPreview, item: &Item, category: Option<String>) {
    core.send(Command::AddFeedItem {
        feed_url: preview.url.clone(),
        item: item.clone(),
        category,
    });
}
