use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, VecDeque};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Track {
    pub id: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub duration: String,
}

#[derive(Default)]
pub struct Queue {
    pub current: Option<Track>,
    pub upcoming: VecDeque<Track>,
}

impl Queue {
    pub fn prepend(&mut self, tracks: Vec<Track>) {
        for track in tracks.into_iter().rev() {
            self.upcoming.push_front(track);
        }
    }

    pub fn remove_indices(&mut self, indices: &BTreeSet<usize>) -> Vec<Track> {
        let mut removed = Vec::new();
        let mut index = 0;
        self.upcoming.retain(|track| {
            let keep = !indices.contains(&index);
            if !keep {
                removed.push(track.clone());
            }
            index += 1;
            keep
        });
        removed
    }

    pub fn move_indices(&mut self, indices: &BTreeSet<usize>, down: bool) -> BTreeSet<usize> {
        let mut moved = indices.clone();
        let mut order: Vec<usize> = indices.iter().copied().collect();
        if down {
            order.reverse();
        }
        for index in order {
            let target = if down {
                index + 1
            } else {
                index.saturating_sub(1)
            };
            if target != index
                && index < self.upcoming.len()
                && target < self.upcoming.len()
                && !moved.contains(&target)
            {
                self.upcoming.swap(index, target);
                moved.remove(&index);
                moved.insert(target);
            }
        }
        moved
    }

    pub fn advance(&mut self) -> Option<Track> {
        self.current = self.upcoming.pop_front();
        self.current.clone()
    }
}

#[cfg(test)]
pub fn track(id: &str) -> Track {
    Track {
        id: id.into(),
        title: id.into(),
        artist: "Artist".into(),
        album: String::new(),
        duration: "3:00".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn moving_multiple_entries_preserves_relative_order_and_stops_at_edges() {
        let mut queue = Queue::default();
        queue
            .upcoming
            .extend([track("a"), track("b"), track("c"), track("d"), track("e")]);
        let marked = queue.move_indices(&BTreeSet::from([1, 2, 4]), false);
        assert_eq!(marked, BTreeSet::from([0, 1, 3]));
        assert_eq!(
            queue.upcoming,
            [track("b"), track("c"), track("a"), track("e"), track("d")]
        );
        let marked = queue.move_indices(&marked, false);
        assert_eq!(marked, BTreeSet::from([0, 1, 2]));
        let marked = queue.move_indices(&marked, true);
        assert_eq!(marked, BTreeSet::from([1, 2, 3]));
        assert_eq!(
            queue.upcoming,
            [track("a"), track("b"), track("c"), track("e"), track("d")]
        );
    }

    #[test]
    fn play_next_and_reorder_preserve_remaining_queue() {
        let mut queue = Queue::default();
        queue.upcoming.extend([track("a"), track("b"), track("c")]);
        assert_eq!(queue.advance(), Some(track("a")));
        queue.upcoming.push_front(track("next"));
        assert_eq!(queue.advance(), Some(track("next")));
        assert_eq!(
            queue.move_indices(&BTreeSet::from([1]), false),
            BTreeSet::from([0])
        );
        assert_eq!(queue.advance(), Some(track("c")));
        assert_eq!(queue.advance(), Some(track("b")));
        assert_eq!(queue.advance(), None);
        assert_eq!(queue.current, None);
    }
    #[test]
    fn removing_selected_only_removes_that_entry() {
        let mut queue = Queue::default();
        queue.upcoming.extend([track("a"), track("b"), track("c")]);
        assert_eq!(queue.remove_indices(&BTreeSet::from([1])), vec![track("b")]);
        assert_eq!(queue.upcoming, VecDeque::from([track("a"), track("c")]));
        assert!(queue.remove_indices(&BTreeSet::from([9])).is_empty());
        assert_eq!(queue.current, None);
    }
}
