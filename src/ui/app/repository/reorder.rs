use crate::ui::app::Foxy;
use log::info;

impl Foxy {
    /// Move a repository from `from_idx` to `to_idx` in the flat repositories Vec.
    /// Updates all index-based fields that reference repositories by position.
    pub(crate) fn reorder_repository(&mut self, from_idx: usize, to_idx: usize) {
        let repos = &mut self.repository_view_state.repositories;
        let len = repos.len();
        if from_idx >= len || to_idx >= len || from_idx == to_idx {
            return;
        }

        let repo_name = repos[from_idx].name.clone();
        let repo = repos.remove(from_idx);
        repos.insert(to_idx, repo);

        info!(
            "Reordered repository '{}' from index {} to {}",
            repo_name, from_idx, to_idx
        );

        // Update all index-based fields to follow the move
        self.repository_view_state.selected_repository = Self::adjust_index_after_move(
            self.repository_view_state.selected_repository,
            from_idx,
            to_idx,
        );
        self.syncing_repository =
            Self::adjust_index_after_move(self.syncing_repository, from_idx, to_idx);
        self.update_ready_repo =
            Self::adjust_index_after_move(self.update_ready_repo, from_idx, to_idx);
        self.download_finished_repo =
            Self::adjust_index_after_move(self.download_finished_repo, from_idx, to_idx);
        self.selected_repository_for_settings =
            Self::adjust_index_after_move(self.selected_repository_for_settings, from_idx, to_idx);

        self.save_repositories();
    }

    /// Convert a visual insertion slot (`0..=len`) into the final Vec index used after removal.
    ///
    /// Drag-and-drop works with slots between rows, while `reorder_repository` expects the
    /// destination index in the post-removal Vec. Moving downward therefore shifts the target
    /// left by one after the source row is removed.
    pub(crate) fn repository_drop_target_index(
        from_idx: usize,
        insert_slot: usize,
        len: usize,
    ) -> Option<usize> {
        if from_idx >= len || insert_slot > len {
            return None;
        }

        let target_idx = if insert_slot > from_idx {
            insert_slot.saturating_sub(1)
        } else {
            insert_slot
        };

        (target_idx < len && target_idx != from_idx).then_some(target_idx)
    }

    /// Given an optional index referencing a repository, adjust it after a move
    /// from `from_idx` to `to_idx` (element was removed then inserted).
    fn adjust_index_after_move(
        index: Option<usize>,
        from_idx: usize,
        to_idx: usize,
    ) -> Option<usize> {
        let idx = index?;
        if idx == from_idx {
            // The tracked item was the one that moved
            return Some(to_idx);
        }
        let mut adjusted = idx;
        // Removing from `from_idx` shifts items after it down by 1
        if idx > from_idx {
            adjusted -= 1;
        }
        // Inserting at `to_idx` shifts items at or after it up by 1
        if adjusted >= to_idx {
            adjusted += 1;
        }
        Some(adjusted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adjust_index_tracks_moved_item() {
        // Moving item 2 to position 0
        assert_eq!(Foxy::adjust_index_after_move(Some(2), 2, 0), Some(0));
    }

    #[test]
    fn adjust_index_shifts_items_between() {
        // Moving item 0 to position 3: items 1,2,3 shift down, then insert at 3
        // Item at 1 -> after remove: 0, after insert at 3: 0
        assert_eq!(Foxy::adjust_index_after_move(Some(1), 0, 3), Some(0));
        // Item at 3 -> after remove: 2, after insert at 3: 2
        assert_eq!(Foxy::adjust_index_after_move(Some(3), 0, 3), Some(2));
    }

    #[test]
    fn adjust_index_none_stays_none() {
        assert_eq!(Foxy::adjust_index_after_move(None, 1, 3), None);
    }

    #[test]
    fn adjust_index_unaffected_item() {
        // Moving item 3 to 1; item at 0 is unaffected
        assert_eq!(Foxy::adjust_index_after_move(Some(0), 3, 1), Some(0));
    }

    #[test]
    fn adjust_index_item_after_source_before_target() {
        // Moving item 1 to 3: item at index 2 -> after remove of 1: 1, after insert at 3: 1
        assert_eq!(Foxy::adjust_index_after_move(Some(2), 1, 3), Some(1));
    }

    #[test]
    fn adjust_index_move_down_adjacent() {
        // Moving item 0 to 1: item at 0 -> 1, item at 1 -> after remove: 0, insert at 1: 0
        assert_eq!(Foxy::adjust_index_after_move(Some(0), 0, 1), Some(1));
        assert_eq!(Foxy::adjust_index_after_move(Some(1), 0, 1), Some(0));
    }

    #[test]
    fn adjust_index_move_up_adjacent() {
        // Moving item 1 to 0: item at 1 -> 0, item at 0 -> after remove: still 0, insert at 0: 1
        assert_eq!(Foxy::adjust_index_after_move(Some(1), 1, 0), Some(0));
        assert_eq!(Foxy::adjust_index_after_move(Some(0), 1, 0), Some(1));
    }

    #[test]
    fn adjust_index_same_from_to_returns_same() {
        assert_eq!(Foxy::adjust_index_after_move(Some(3), 2, 2), Some(3));
        assert_eq!(Foxy::adjust_index_after_move(Some(2), 2, 2), Some(2));
    }

    #[test]
    fn adjust_index_move_last_to_first() {
        // In a 5-element list, move index 4 to 0
        // Item at 0 -> after remove of 4: 0, after insert at 0: 1
        assert_eq!(Foxy::adjust_index_after_move(Some(0), 4, 0), Some(1));
        // Item at 4 -> moved to 0
        assert_eq!(Foxy::adjust_index_after_move(Some(4), 4, 0), Some(0));
        // Item at 2 -> after remove of 4: 2, after insert at 0: 3
        assert_eq!(Foxy::adjust_index_after_move(Some(2), 4, 0), Some(3));
    }

    #[test]
    fn adjust_index_move_first_to_last() {
        // In a 5-element list, move index 0 to 4
        // Item at 0 -> moved to 4
        assert_eq!(Foxy::adjust_index_after_move(Some(0), 0, 4), Some(4));
        // Item at 1 -> after remove of 0: 0, insert at 4: 0
        assert_eq!(Foxy::adjust_index_after_move(Some(1), 0, 4), Some(0));
        // Item at 4 -> after remove of 0: 3, after insert at 4: 3
        assert_eq!(Foxy::adjust_index_after_move(Some(4), 0, 4), Some(3));
    }

    #[test]
    fn adjust_index_far_item_unaffected_by_nearby_move() {
        // Move 1 to 2, item at 5 should stay at 5
        // Item at 5 -> after remove of 1: 4, after insert at 2: 4 (since 4 < 2 is false; 4 >= 2 so +1 = 5)
        assert_eq!(Foxy::adjust_index_after_move(Some(5), 1, 2), Some(5));
    }

    #[test]
    fn drop_target_moves_repository_up() {
        assert_eq!(Foxy::repository_drop_target_index(3, 1, 5), Some(1));
    }

    #[test]
    fn drop_target_moves_repository_down() {
        assert_eq!(Foxy::repository_drop_target_index(1, 4, 5), Some(3));
    }

    #[test]
    fn drop_target_supports_insert_after_last_row() {
        assert_eq!(Foxy::repository_drop_target_index(1, 5, 5), Some(4));
    }

    #[test]
    fn drop_target_ignores_same_position_slots() {
        assert_eq!(Foxy::repository_drop_target_index(2, 2, 5), None);
        assert_eq!(Foxy::repository_drop_target_index(2, 3, 5), None);
    }

    #[test]
    fn drop_target_rejects_out_of_bounds_input() {
        assert_eq!(Foxy::repository_drop_target_index(5, 1, 5), None);
        assert_eq!(Foxy::repository_drop_target_index(1, 6, 5), None);
    }
}
