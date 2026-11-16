//! Showing a feed's items before anything is downloaded from it, and downloading one of
//! them by hand.
use super::{fetch_feed, FeedDone, Worker};
use crate::episodes;
use crate::feeds::{self, Item};
use crate::intake::{self, Source};
use crate::model::FeedPreview;

/// A feed is someone else's text: its links may only be magnet links or web addresses,
/// never a path to a file on this computer.
pub(super) fn is_link(link: &str) -> bool {
    matches!(intake::parse(link), Ok(Source::Link(_)))
}

/// More than any feed needs to show; the rest are left out.
const MAX_SHOWN: usize = 300;

impl Worker {
    pub(super) fn preview_feed(&mut self, url: Option<String>) {
        let Some(url) = url.map(|u| u.trim().to_owned()).filter(|u| !u.is_empty()) else {
            self.preview = None;
            return;
        };
        let mut preview = FeedPreview {
            url: url.clone(),
            loading: true,
            ..Default::default()
        };
        let Some(http) = self.http.clone() else {
            preview.loading = false;
            preview.error = Some("Feeds cannot be read while all traffic is blocked.".into());
            self.preview = Some(preview);
            return;
        };
        self.preview = Some(preview);
        let tx = self.feed_tx.clone();
        tokio::spawn(async move {
            let result = fetch_feed(&http, &url).await;
            let _ = tx.send(FeedDone {
                url,
                result,
                preview: true,
            });
        });
    }

    pub(super) fn preview_done(&mut self, done: FeedDone) {
        // Closed, or another feed asked for since.
        let Some(preview) = self.preview.as_mut().filter(|p| p.url == done.url) else {
            return;
        };
        preview.loading = false;
        match done.result {
            Ok(text) => {
                let mut items = feeds::parse(&text);
                items.truncate(MAX_SHOWN);
                let state = self.store.data.feed_state.get(&done.url);
                if let Some(state) = state {
                    preview.seen = items
                        .iter()
                        .filter(|i| state.seen.contains(&i.id))
                        .map(|i| i.id.clone())
                        .collect();
                    preview.taken = state.episodes.clone();
                    preview.primed = state.primed;
                }
                preview.error = items
                    .is_empty()
                    .then(|| "No items with a link were found in it.".to_owned());
                preview.items = items;
            }
            Err(e) => preview.error = Some(format!("Could not read it: {e}")),
        }
    }

    pub(super) fn add_feed_item(&mut self, feed_url: String, item: Item, category: Option<String>) {
        if !is_link(&item.link) {
            return self.notice(format!("{} has no web or magnet link.", item.title), true);
        }
        let category = category.map(|c| c.trim().to_owned()).filter(|c| !c.is_empty());
        // Remembered only for a saved feed; an unsaved one has nowhere to keep it yet.
        let saved = self.store.data.feeds.iter().find(|f| f.url == feed_url).cloned();
        if let Some(feed) = saved {
            let state = self.store.data.feed_state.entry(feed_url.clone()).or_default();
            if !state.seen.contains(&item.id) {
                state.seen.push(item.id.clone());
            }
            if let Some(key) = feed.skip_repeats.then(|| episodes::find(&item.title)).flatten() {
                if !state.episodes.contains(&key.key()) {
                    state.episodes.push(key.key());
                }
            }
            let (seen, taken) = (state.seen.clone(), state.episodes.clone());
            if let Some(preview) = self.preview.as_mut().filter(|p| p.url == feed_url) {
                preview.seen = seen;
                preview.taken = taken;
            }
        }
        self.notice(format!("Adding {}", item.title), false);
        self.add(item.link, category);
    }
}
