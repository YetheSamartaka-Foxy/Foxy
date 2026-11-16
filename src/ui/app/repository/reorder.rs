use crate::ui::app::Foxy;
use log::info;

impl Foxy {
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

        self.remap_repository_indices(|idx| {
            Self::adjust_index_after_move(Some(idx), from_idx, to_idx)
        });
        self.save_repositories();
    }

    pub(crate) fn remap_repository_indices(&mut self, remap: impl Fn(usize) -> Option<usize>) {
        let remap_opt = |index: Option<usize>| index.and_then(&remap);
        self.repository_view_state.selected_repository =
            remap_opt(self.repository_view_state.selected_repository);
        self.selected_repository_for_settings = remap_opt(self.selected_repository_for_settings);
        self.syncing_repository = remap_opt(self.syncing_repository);
        self.update_ready_repo = remap_opt(self.update_ready_repo);
        self.download_finished_repo = remap_opt(self.download_finished_repo);
        self.download_disk_space_shortfall = self
            .download_disk_space_shortfall
            .take()
            .and_then(|(index, shortfall)| remap(index).map(|index| (index, shortfall)));
        self.update_modal_disk_space_probe = None;
        self.completed_repository_check_banner = self
            .completed_repository_check_banner
            .take()
            .and_then(|mut banner| {
                banner.repo_index = remap(banner.repo_index)?;
                Some(banner)
            });
        self.pending_repo_metadata_refresh = self
            .pending_repo_metadata_refresh
            .iter()
            .filter_map(|index| remap(*index))
            .collect();
        if let Some(action) = self.pending_repository_space_bulk_action.as_mut() {
            action
                .entries
                .retain_mut(|entry| match remap(entry.repo_index) {
                    Some(index) => {
                        entry.repo_index = index;
                        true
                    }
                    None => false,
                });
            if action.entries.is_empty() {
                self.pending_repository_space_bulk_action = None;
            }
        }
        self.remap_addon_selection_indices(&remap);
    }

    pub(crate) fn adjust_index_after_removal(
        index: Option<usize>,
        removed_idx: usize,
    ) -> Option<usize> {
        match index? {
            idx if idx == removed_idx => None,
            idx if idx > removed_idx => Some(idx - 1),
            idx => Some(idx),
        }
    }

    pub(crate) fn adjust_index_after_insertion(index: usize, inserted_idx: usize) -> usize {
        if index >= inserted_idx {
            index + 1
        } else {
            index
        }
    }

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

    fn adjust_index_after_move(
        index: Option<usize>,
        from_idx: usize,
        to_idx: usize,
    ) -> Option<usize> {
        let idx = index?;
        if idx == from_idx {
            return Some(to_idx);
        }
        let mut adjusted = idx;

        if idx > from_idx {
            adjusted -= 1;
        }

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
        assert_eq!(Foxy::adjust_index_after_move(Some(2), 2, 0), Some(0));
    }

    #[test]
    fn adjust_index_shifts_items_between() {
        assert_eq!(Foxy::adjust_index_after_move(Some(1), 0, 3), Some(0));

        assert_eq!(Foxy::adjust_index_after_move(Some(3), 0, 3), Some(2));
    }

    #[test]
    fn adjust_index_none_stays_none() {
        assert_eq!(Foxy::adjust_index_after_move(None, 1, 3), None);
    }

    #[test]
    fn adjust_index_unaffected_item() {
        assert_eq!(Foxy::adjust_index_after_move(Some(0), 3, 1), Some(0));
    }

    #[test]
    fn adjust_index_item_after_source_before_target() {
        assert_eq!(Foxy::adjust_index_after_move(Some(2), 1, 3), Some(1));
    }

    #[test]
    fn adjust_index_move_down_adjacent() {
        assert_eq!(Foxy::adjust_index_after_move(Some(0), 0, 1), Some(1));
        assert_eq!(Foxy::adjust_index_after_move(Some(1), 0, 1), Some(0));
    }

    #[test]
    fn adjust_index_move_up_adjacent() {
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
        assert_eq!(Foxy::adjust_index_after_move(Some(0), 4, 0), Some(1));

        assert_eq!(Foxy::adjust_index_after_move(Some(4), 4, 0), Some(0));

        assert_eq!(Foxy::adjust_index_after_move(Some(2), 4, 0), Some(3));
    }

    #[test]
    fn adjust_index_move_first_to_last() {
        assert_eq!(Foxy::adjust_index_after_move(Some(0), 0, 4), Some(4));

        assert_eq!(Foxy::adjust_index_after_move(Some(1), 0, 4), Some(0));

        assert_eq!(Foxy::adjust_index_after_move(Some(4), 0, 4), Some(3));
    }

    #[test]
    fn adjust_index_far_item_unaffected_by_nearby_move() {
        assert_eq!(Foxy::adjust_index_after_move(Some(5), 1, 2), Some(5));
    }

    #[test]
    fn removal_drops_the_removed_index_and_shifts_later_ones() {
        assert_eq!(Foxy::adjust_index_after_removal(Some(2), 2), None);
        assert_eq!(Foxy::adjust_index_after_removal(Some(5), 2), Some(4));
        assert_eq!(Foxy::adjust_index_after_removal(Some(1), 2), Some(1));
        assert_eq!(Foxy::adjust_index_after_removal(None, 2), None);
    }

    #[test]
    fn insertion_shifts_indices_at_and_after_the_new_row() {
        assert_eq!(Foxy::adjust_index_after_insertion(1, 2), 1);
        assert_eq!(Foxy::adjust_index_after_insertion(2, 2), 3);
        assert_eq!(Foxy::adjust_index_after_insertion(4, 2), 5);
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
