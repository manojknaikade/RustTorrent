//! File priority: high, normal or low. The engine downloads a torrent's files in one
//! order, so priority is turned into that order: high files first, then normal ones, then
//! low ones, each group sorted by name as the engine sorts files by default.
use serde::{Deserialize, Serialize};

use crate::model::FileMeta;

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default, Hash)]
pub enum Priority {
    High,
    #[default]
    Normal,
    Low,
}

impl Priority {
    pub const ALL: [Priority; 3] = [Priority::High, Priority::Normal, Priority::Low];

    pub fn label(self) -> &'static str {
        match self {
            Priority::High => "High",
            Priority::Normal => "Normal",
            Priority::Low => "Low",
        }
    }
}

/// The priority of each file, kept as the files that are not normal.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug, Default)]
#[serde(default)]
pub struct FilePriorities {
    /// Sorted, without repeats.
    pub high: Vec<usize>,
    /// Sorted, without repeats.
    pub low: Vec<usize>,
}

impl FilePriorities {
    pub fn get(&self, file: usize) -> Priority {
        if self.high.binary_search(&file).is_ok() {
            Priority::High
        } else if self.low.binary_search(&file).is_ok() {
            Priority::Low
        } else {
            Priority::Normal
        }
    }

    pub fn set(&mut self, files: &[usize], priority: Priority) {
        self.high.retain(|f| !files.contains(f));
        self.low.retain(|f| !files.contains(f));
        let list = match priority {
            Priority::High => &mut self.high,
            Priority::Low => &mut self.low,
            Priority::Normal => return,
        };
        list.extend_from_slice(files);
        list.sort_unstable();
        list.dedup();
    }

    pub fn is_empty(&self) -> bool {
        self.high.is_empty() && self.low.is_empty()
    }

    /// The order to give the engine, or `None` for its own order (all files normal).
    /// Indexes past the end of `files` are left out.
    pub fn file_order(&self, files: &[FileMeta]) -> Option<Vec<usize>> {
        if files.is_empty() || !(self.high.iter().chain(&self.low)).any(|&f| f < files.len()) {
            return None;
        }
        let mut order: Vec<usize> = (0..files.len()).collect();
        order.sort_by(|&a, &b| {
            let rank = |f: usize| match self.get(f) {
                Priority::High => 0,
                Priority::Normal => 1,
                Priority::Low => 2,
            };
            rank(a).cmp(&rank(b)).then_with(|| files[a].name.cmp(&files[b].name))
        });
        Some(order)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn files(names: &[&str]) -> Vec<FileMeta> {
        names
            .iter()
            .map(|n| FileMeta {
                name: n.to_string(),
                len: 1,
            })
            .collect()
    }

    #[test]
    fn all_normal_keeps_the_engine_order() {
        let p = FilePriorities::default();
        assert_eq!(p.file_order(&files(&["b", "a"])), None);
        assert_eq!(p.get(0), Priority::Normal);
    }

    #[test]
    fn high_first_low_last_each_by_name() {
        let mut p = FilePriorities::default();
        p.set(&[3, 0], Priority::High);
        p.set(&[1], Priority::Low);
        // Names: 0 "d", 1 "a", 2 "c", 3 "b", 4 "e".
        let order = p.file_order(&files(&["d", "a", "c", "b", "e"])).unwrap();
        assert_eq!(order, vec![3, 0, 2, 4, 1]);
    }

    #[test]
    fn setting_again_moves_a_file_between_groups() {
        let mut p = FilePriorities::default();
        p.set(&[2, 1, 2], Priority::High);
        assert_eq!(p.high, vec![1, 2]);
        p.set(&[2], Priority::Low);
        assert_eq!((p.high.clone(), p.low.clone()), (vec![1], vec![2]));
        p.set(&[1, 2], Priority::Normal);
        assert!(p.is_empty());
    }

    #[test]
    fn indexes_past_the_end_are_ignored() {
        let mut p = FilePriorities::default();
        p.set(&[7], Priority::High);
        assert_eq!(p.file_order(&files(&["a"])), None);
    }
}
