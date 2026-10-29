use eframe::egui;

use crate::core::tasks::calculate_hashes::{HashEvidenceState, HashIssueKind, StalledReadDecision};
use crate::ui::app::Foxy;

impl Foxy {
    pub(crate) fn has_pending_hash_save(&self) -> bool {
        self.hash_evidence_state
            .as_ref()
            .is_some_and(|state| state.has_retry_save_pending())
    }

    pub(crate) fn render_integrity_recheck_prompt(&mut self, ctx: &egui::Context) {
        let Some((address, path)) = self.pending_integrity_recheck_prompt.clone() else {
            return;
        };
        let Some(repo) = self.repository_view_state.repositories.iter().find(|repo| {
            Self::normalize_repo_url(&repo.address) == Self::normalize_repo_url(&address)
                && Self::repo_instance_path_key(&repo.path) == Self::repo_instance_path_key(&path)
        }) else {
            self.pending_integrity_recheck_prompt = None;
            return;
        };
        let repo_name = repo.name.clone();
        let mut open = true;
        let mut replace = false;
        let mut keep_current = false;
        egui::Window::new(self.t("Run full integrity recheck?"))
            .id(egui::Id::unique("replace-quick-check-with-integrity"))
            .collapsible(false)
            .resizable(false)
            .open(&mut open)
            .show(ctx, |ui| {
                ui.label(self.t_fmt(
                    "A quick local check is running for {name}. Stop it after it settles and start a full integrity recheck?",
                    &[("name", repo_name)],
                ));
                ui.horizontal(|ui| {
                    if ui.button(self.t("Stop check and run integrity recheck")).clicked() {
                        replace = true;
                    }
                    if ui.button(self.t("Keep current check")).clicked() {
                        keep_current = true;
                    }
                });
            });

        if replace {
            self.integrity_recheck_after_settlement = Some((address, path));
            self.pending_integrity_recheck_prompt = None;
            self.cancel_sync();
        } else if keep_current || !open {
            self.pending_integrity_recheck_prompt = None;
        }
    }

    pub(crate) fn poll_hash_evidence_retry(&mut self) {
        while let Ok(result) = self.hash_evidence_retry_rx.try_recv() {
            self.hash_evidence_retry_in_flight = false;
            match result {
                Ok(true) => {
                    self.show_success_toast(self.t("Verification evidence saved"));
                    if self
                        .hash_evidence_state
                        .as_ref()
                        .is_some_and(|state| !state.is_incomplete())
                    {
                        self.hash_evidence_notice_open = false;
                    }
                }
                Ok(false) => self.show_error_toast(self.t("No pending verification save to retry")),
                Err(error) => {
                    log::warn!("Retrying verification evidence save failed: {error}");
                    self.show_error_toast(self.t("Could not save verification evidence. Retry after freeing space or resolving the database error."));
                }
            }
            self.needs_repaint = true;
        }
    }

    pub(crate) fn render_hash_evidence_notice(&mut self, ctx: &egui::Context) {
        let Some(state) = self.hash_evidence_state.clone() else {
            return;
        };
        let issues = state.issues_snapshot();
        let stalled = state.stalled_snapshot();
        let has_retry_save = state.has_retry_save_pending();
        let current_notice_state = (issues.len(), stalled.len(), has_retry_save);
        if current_notice_state.0 > self.hash_evidence_notice_seen.0
            || current_notice_state.1 > self.hash_evidence_notice_seen.1
            || (current_notice_state.2 && !self.hash_evidence_notice_seen.2)
        {
            self.hash_evidence_notice_open = true;
        }
        self.hash_evidence_notice_seen = current_notice_state;
        let pending_stalls = stalled
            .iter()
            .filter(|read| !self.hash_stall_acknowledged.contains(&read.file_path))
            .cloned()
            .collect::<Vec<_>>();
        if issues.is_empty() && pending_stalls.is_empty() && !has_retry_save {
            return;
        }
        if !self.hash_evidence_notice_open {
            return;
        }

        let mut open = self.hash_evidence_notice_open;
        let mut discard = false;
        egui::Window::new(self.t("Integrity check needs attention"))
            .id(egui::Id::unique("hash-evidence-notice"))
            .open(&mut open)
            .resizable(true)
            .show(ctx, |ui| {
                if let Some(read) = pending_stalls.first() {
                    ui.label(self.t_fmt(
                        "This read has made no progress for {seconds} seconds: {file}",
                        &[
                            ("seconds", read.no_progress_secs.to_string()),
                            ("file", read.file_path.clone()),
                        ],
                    ));
                    ui.horizontal(|ui| {
                        if ui.button(self.t("Keep waiting")).clicked() {
                            state.respond_to_stall(&read.file_path, StalledReadDecision::Continue);
                            self.hash_stall_acknowledged.insert(read.file_path.clone());
                        }
                        if ui.button(self.t("Skip this file")).clicked() {
                            state.respond_to_stall(&read.file_path, StalledReadDecision::Skip);
                            self.hash_stall_acknowledged.insert(read.file_path.clone());
                        }
                        if ui.button(self.t("Cancel integrity check")).clicked() {
                            state.respond_to_stall(&read.file_path, StalledReadDecision::Cancel);
                            self.hash_stall_acknowledged.insert(read.file_path.clone());
                        }
                    });
                }

                if !issues.is_empty() {
                    ui.separator();
                    ui.label(self.t("This check did not verify every file or save all evidence."));
                    for issue in issues.iter().take(8) {
                        let kind = match &issue.kind {
                            HashIssueKind::Missing => self.t("Missing"),
                            HashIssueKind::Unreadable => self.t("Unreadable"),
                            HashIssueKind::Unstable => self.t("Changed while reading"),
                            HashIssueKind::SaveFailed => self.t("Could not save"),
                            HashIssueKind::Skipped => self.t("Skipped"),
                        };
                        ui.label(format!("{kind}: {}", issue.file_path));
                        if let Some(detail) = &issue.detail {
                            ui.label(detail);
                        }
                    }
                    if issues.len() > 8 {
                        ui.label(self.t_fmt(
                            "And {count} more file issues",
                            &[("count", (issues.len() - 8).to_string())],
                        ));
                    }
                }

                if has_retry_save {
                    ui.separator();
                    if ui
                        .add_enabled(
                            !self.hash_evidence_retry_in_flight
                                && !self.repository_sync_active()
                                && !self.is_direct_download_running(),
                            egui::Button::new(if self.hash_evidence_retry_in_flight {
                                self.t("Saving verification evidence...")
                            } else {
                                self.t("Retry save")
                            }),
                        )
                        .clicked()
                    {
                        self.start_hash_evidence_retry(state.clone());
                    }
                    if ui
                        .add_enabled(
                            !self.hash_evidence_retry_in_flight
                                && !self.repository_sync_active()
                                && !self.is_direct_download_running(),
                            egui::Button::new(self.t("Discard")),
                        )
                        .clicked()
                    {
                        discard = true;
                    }
                }
            });
        if discard {
            self.hash_evidence_state = None;
        }
        self.hash_evidence_notice_open = open && !discard;
    }

    fn start_hash_evidence_retry(&mut self, state: std::sync::Arc<HashEvidenceState>) {
        if self.hash_evidence_retry_in_flight
            || self.repository_sync_active()
            || self.is_direct_download_running()
        {
            return;
        }
        self.hash_evidence_retry_in_flight = true;
        let tx = self.hash_evidence_retry_tx.clone();
        std::thread::spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                crate::core::api::background_runtime()
                    .map(|runtime| runtime.block_on(state.retry_save_pending()))
                    .unwrap_or_else(|| Err("Background runtime is unavailable".to_owned()))
            }))
            .unwrap_or_else(|_| Err("Verification save worker stopped".to_owned()));
            let _ = tx.send(result);
        });
    }
}
