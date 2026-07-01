use super::*;

impl Foxy {
    pub(super) fn build_update_manifest_text(&self, total_bytes: u64) -> String {
        let repository_name = self
            .repository_view_state
            .selected_repository
            .and_then(|repo_index| self.repository_view_state.repositories.get(repo_index))
            .map(|repo| repo.name.clone())
            .unwrap_or_else(|| self.t("Repository"));

        let mut lines = Vec::with_capacity(self.update_modal_sorted_mod_indices.len() + 1);
        lines.push(format!(
            "# {} - {}",
            repository_name,
            fmt_bytes(total_bytes)
        ));

        for &mod_idx in &self.update_modal_sorted_mod_indices {
            let mod_summary = &self.mod_diff_cache[mod_idx];
            if !mod_summary.needs_update {
                continue;
            }
            lines.push(format!(
                "- {} - {}",
                mod_summary.name,
                fmt_bytes(mod_summary.total_bytes)
            ));
        }

        lines.join("\n")
    }

    pub(super) fn export_update_manifest_to_clipboard(&mut self, total_bytes: u64) {
        let manifest = self.build_update_manifest_text(total_bytes);
        match Clipboard::new().and_then(|mut clipboard| clipboard.set_text(manifest)) {
            Ok(_) => {
                info!("Update manifest copied to clipboard.");
                self.show_success_toast(self.t("Update manifest copied to clipboard."));
            }
            Err(err) => {
                warn!("Failed to copy update manifest: {}", err);
                self.show_error_toast(self.t("Failed to copy update manifest."));
            }
        }
    }

    pub(super) fn export_update_manifest_to_file(&mut self, total_bytes: u64) {
        let manifest = self.build_update_manifest_text(total_bytes);

        let repo_name = self
            .repository_view_state
            .selected_repository
            .and_then(|repo_index| self.repository_view_state.repositories.get(repo_index))
            .map(|repo| repo.name.clone())
            .unwrap_or_else(|| "update_manifest".to_string());

        let safe_name = repo_name
            .chars()
            .map(|c| {
                if c.is_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .collect::<String>();

        if let Some(path) = crate::ui::app::agent_support::save_file(|| {
            FileDialog::new()
                .set_file_name(format!("{}_update_manifest.txt", safe_name))
                .add_filter("Text files", &["txt"])
                .add_filter("All files", &["*"])
                .save_file()
        }) {
            match std::fs::write(&path, &manifest) {
                Ok(()) => {
                    info!(
                        "Update manifest saved to file: {}",
                        crate::core::utils::format::sanitize_log_path(&path)
                    );
                    self.show_success_toast(self.t("Update manifest saved to file."));
                }
                Err(err) => {
                    warn!("Failed to save update manifest to file: {}", err);
                    self.show_error_toast(self.t("Failed to save update manifest to file."));
                }
            }
        }
    }

    pub(super) fn rebuild_update_modal_sort_cache_if_needed(&mut self) {
        let updatable_indices: Vec<usize> = self
            .mod_diff_cache
            .iter()
            .enumerate()
            .filter_map(|(idx, mod_summary)| {
                let is_finished = self
                    .mod_download_progress
                    .get(&mod_summary.name)
                    .is_some_and(|(pct, ..)| *pct >= 1.0);
                (mod_summary.needs_update || is_finished).then_some(idx)
            })
            .collect();
        let mods_len = updatable_indices.len();
        let cache_shape_changed = self.update_modal_sorted_mod_indices.len() != mods_len
            || self.update_modal_mod_name_lowers.len() != mods_len;
        let generation_changed =
            self.update_modal_sorted_generation != self.update_modal_sort_generation;

        if !cache_shape_changed && !generation_changed {
            return;
        }

        let mut sorted_entries: Vec<(usize, u8, f32, String)> = updatable_indices
            .into_iter()
            .map(|idx| {
                let mod_name = &self.mod_diff_cache[idx].name;
                let name_lower = mod_name.to_lowercase();
                let rank = self.mod_download_sort_rank(mod_name);
                let pct = self
                    .mod_download_progress
                    .get(mod_name.as_str())
                    .map(|(p, ..)| *p)
                    .unwrap_or(0.0);
                (idx, rank, pct, name_lower)
            })
            .collect();
        sorted_entries.sort_by(|a, b| {
            a.1.cmp(&b.1)
                .then_with(|| {
                    // Within the same rank, sort by percentage descending
                    // so the mod with highest progress appears first.
                    b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal)
                })
                .then_with(|| locale_compare(&a.3, &b.3))
        });

        self.update_modal_mod_name_lowers = sorted_entries
            .iter()
            .map(|(_, _, _, name)| name.clone())
            .collect();
        self.update_modal_sorted_mod_indices =
            sorted_entries.iter().map(|(idx, _, _, _)| *idx).collect();
        self.update_modal_sorted_generation = self.update_modal_sort_generation;
    }
}
