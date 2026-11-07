use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use crate::core::api::{ProgressEvent, SyncMode};
use crate::core::utils::update_estimate::{UpdateEstimate, UpdatePhase, UpdateWork};
use crate::ui::app::Foxy;
use crate::ui::i18n::{fmt_date, fmt_duration};
use crate::ui::types::RepoInstanceKey;

#[derive(Default)]
pub(crate) struct CompletionEstimateState {
    active: Option<ActiveEstimate>,
    pub(crate) prepared: HashMap<String, PreparedEstimate>,
}

struct ActiveEstimate {
    instance: String,
    tracker: UpdateEstimate,
}

pub(crate) struct PreparedEstimate {
    work: UpdateWork,
    selection: u64,
}

#[derive(Debug, PartialEq)]
pub(crate) struct CompletionForecast {
    pub(crate) remaining: Option<Duration>,
    pub(crate) known: usize,
    pub(crate) unknown: usize,
}

fn combine_estimates(
    active: Option<Duration>,
    queued: impl Iterator<Item = Option<Duration>>,
) -> CompletionForecast {
    let mut forecast = CompletionForecast {
        remaining: active,
        known: 0,
        unknown: 0,
    };
    for remaining in queued {
        if let Some(remaining) = remaining {
            forecast.known += 1;
            forecast.remaining = forecast
                .remaining
                .and_then(|total| total.checked_add(remaining));
        } else {
            forecast.unknown += 1;
            forecast.remaining = None;
        }
    }
    forecast
}

impl Foxy {
    pub(in crate::ui::app) fn begin_update_completion_estimate(&mut self, repo_index: usize) {
        self.completion_estimate.active = None;
        let Some(repo) = self.repository_view_state.repositories.get(repo_index) else {
            return;
        };
        let instance = Self::repo_instance_key(&repo.address, &repo.path);
        if self.current_sync_mode != Some(SyncMode::Download) {
            self.completion_estimate.prepared.remove(&instance);
            return;
        }
        let mut tracker = UpdateEstimate::new(Instant::now(), repo.update_timing.clone());
        if let Some(prepared) = self.completion_estimate.prepared.get(&instance)
            && prepared.selection == Self::addon_selection_fingerprint(repo)
        {
            tracker.set_work(prepared.work);
        }
        self.completion_estimate.active = Some(ActiveEstimate { instance, tracker });
    }

    pub(in crate::ui::app) fn observe_update_completion_event(&mut self, event: &ProgressEvent) {
        if let ProgressEvent::DownloadPlan {
            files_total,
            planned_bytes,
            full_bytes,
            ..
        } = event
        {
            let work = UpdateWork {
                files: *files_total,
                transfer_bytes: *planned_bytes,
                full_bytes: Some(*full_bytes),
            };
            if let Some(active) = self.completion_estimate.active.as_mut() {
                active.tracker.set_work(work);
            }
            if let Some(repo) = self
                .syncing_repository
                .and_then(|index| self.repository_view_state.repositories.get(index))
            {
                let key = Self::repo_instance_key(&repo.address, &repo.path);
                let normal_selection = Self::resolve_selected_mod_states(repo, None);
                if self.active_sync_selection.as_ref() == Some(&normal_selection) {
                    self.completion_estimate.prepared.insert(
                        key,
                        PreparedEstimate {
                            work,
                            selection: Self::addon_selection_fingerprint(repo),
                        },
                    );
                } else {
                    self.completion_estimate.prepared.remove(&key);
                }
            }
        }
        if self.current_sync_mode != Some(SyncMode::Download) {
            return;
        }
        match event {
            ProgressEvent::UpdatePhase(phase) => {
                if let Some(active) = self.completion_estimate.active.as_mut() {
                    active.tracker.set_phase(*phase, Instant::now());
                }
                self.download_eta_remaining = None;
                self.download_eta_updated_at = None;
            }
            ProgressEvent::RetryableDownloadFailure => self.invalidate_update_completion_estimate(),
            ProgressEvent::DownloadMod {
                mod_name, attempt, ..
            } if self
                .mod_download_attempts
                .get(mod_name)
                .is_some_and(|previous| attempt > previous) =>
            {
                self.invalidate_update_completion_estimate();
            }
            ProgressEvent::Stage { label, .. }
                if label == "Cancelling..." || label == "Reverting changes" =>
            {
                self.invalidate_update_completion_estimate();
            }
            _ => {}
        }
    }

    pub(in crate::ui::app) fn invalidate_update_completion_estimate(&mut self) {
        if let Some(active) = self.completion_estimate.active.as_mut() {
            active.tracker.invalidate();
        }
        self.download_eta_remaining = None;
        self.download_eta_updated_at = None;
    }

    pub(in crate::ui::app) fn pause_update_completion_estimate(&mut self, paused: bool) {
        if let Some(active) = self.completion_estimate.active.as_mut() {
            active.tracker.set_paused(paused, Instant::now());
        }
    }

    pub(in crate::ui::app) fn finish_update_completion_estimate(&mut self, success: bool) {
        let Some(mut active) = self.completion_estimate.active.take() else {
            return;
        };
        self.completion_estimate.prepared.remove(&active.instance);
        if !success {
            return;
        }
        let Some(timing) = active.tracker.finish(Instant::now()) else {
            return;
        };
        if let Some(repo) = self
            .repository_view_state
            .repositories
            .iter_mut()
            .find(|repo| Self::repo_instance_key(&repo.address, &repo.path) == active.instance)
        {
            repo.update_timing = Some(timing);
            self.mark_repositories_dirty();
        }
    }

    pub(crate) fn update_completion_phase(&self) -> Option<UpdatePhase> {
        self.completion_estimate
            .active
            .as_ref()
            .map(|active| active.tracker.phase())
    }

    pub(crate) fn update_completion_forecast(&self) -> CompletionForecast {
        let active = self.completion_estimate.active.as_ref().and_then(|active| {
            active
                .tracker
                .remaining(Instant::now(), self.download_eta_remaining)
        });
        let mut seen = HashSet::new();
        let queued = self
            .repository_space_sync_queue
            .iter()
            .filter(|entry| entry.mode == SyncMode::Download)
            .map(|entry| &entry.target)
            .chain(
                self.repository_visual_folder_sync_queue
                    .iter()
                    .filter(|entry| entry.mode == SyncMode::Download)
                    .map(|entry| &entry.target),
            )
            .chain(
                self.scheduled_update_estimate_targets()
                    .into_iter()
                    .flatten(),
            )
            .filter(|target| {
                seen.insert(Self::repo_instance_key(
                    &target.remote_url,
                    &target.local_path,
                ))
            })
            .map(|target| self.queued_update_completion_estimate(target));
        combine_estimates(active, queued)
    }

    fn queued_update_completion_estimate(&self, target: &RepoInstanceKey) -> Option<Duration> {
        let index = self.repo_index_by_instance(
            &Self::normalize_repo_url(&target.remote_url),
            &Self::repo_instance_path_key(&target.local_path),
        )?;
        let repo = self.repository_view_state.repositories.get(index)?;
        if self.is_repository_paused(repo) {
            return None;
        }
        let key = Self::repo_instance_key(&repo.address, &repo.path);
        let prepared = self.completion_estimate.prepared.get(&key)?;
        if prepared.selection != Self::addon_selection_fingerprint(repo) {
            return None;
        }
        repo.update_timing.as_ref()?.estimate(prepared.work)
    }

    pub(crate) fn render_update_completion_estimate(&self, ui: &mut egui::Ui) {
        let forecast = self.update_completion_forecast();
        let text = if self.download_paused {
            self.t("Completion estimate paused")
        } else if let Some(remaining) = forecast.remaining
            && let Ok(delta) = chrono::Duration::from_std(remaining)
            && let Some(finish) = chrono::Local::now().checked_add_signed(delta)
        {
            self.t_fmt(
                "Estimated completion: {time} (about {remaining} remaining)",
                &[
                    ("time", fmt_date(finish.timestamp().max(0) as u64)),
                    ("remaining", fmt_duration(remaining)),
                ],
            )
        } else {
            self.t("Estimated completion: unknown")
        };
        ui.add(egui::Label::new(egui::RichText::new(text).strong()).wrap());
        if forecast.known + forecast.unknown > 0 {
            ui.add(
                egui::Label::new(self.t_fmt(
                    "Queued updates: {known} estimated, {unknown} unknown",
                    &[
                        ("known", forecast.known.to_string()),
                        ("unknown", forecast.unknown.to_string()),
                    ],
                ))
                .wrap(),
            );
        }
        ui.add(egui::Label::new(self.t("Includes preparation, transfer, patching, verification, saving, and queued updates. Estimates use timings from previous successful updates of each installation.")).wrap());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queue_estimate_includes_every_known_target() {
        let forecast = combine_estimates(
            Some(Duration::from_secs(10)),
            [Some(Duration::from_secs(20)), Some(Duration::from_secs(30))].into_iter(),
        );
        assert_eq!(forecast.remaining, Some(Duration::from_secs(60)));
        assert_eq!((forecast.known, forecast.unknown), (2, 0));
    }

    #[test]
    fn unknown_target_or_overflow_prevents_whole_queue_claim() {
        let forecast = combine_estimates(
            Some(Duration::from_secs(10)),
            [None, Some(Duration::from_secs(30))].into_iter(),
        );
        assert_eq!(forecast.remaining, None);
        assert_eq!((forecast.known, forecast.unknown), (1, 1));
        assert_eq!(
            combine_estimates(None, [Some(Duration::from_secs(3))].into_iter()).remaining,
            None
        );
        assert_eq!(
            combine_estimates(
                Some(Duration::MAX),
                [Some(Duration::from_secs(1))].into_iter()
            )
            .remaining,
            None
        );
    }
}
