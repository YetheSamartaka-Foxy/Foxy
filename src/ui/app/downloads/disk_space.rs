use std::path::Path;
use std::time::{Duration, Instant};

use crate::core::utils::disk_space::{
    DISK_SPACE_MARGIN_BYTES, DiskSpaceShortfall, probe_disk_space_shortfall,
};
use crate::ui::app::Foxy;
use crate::ui::i18n::fmt_bytes;

/// How long the update modal trusts one free-space probe. Short enough that
/// freeing space on the drive clears the notice while the modal stays open.
const PROBE_TTL: Duration = Duration::from_secs(3);

/// The last free-space probe the update modal ran, keyed by what it probed so
/// a repaint reuses it instead of hitting the volume every frame.
#[derive(Clone, Debug)]
pub struct UpdateModalDiskSpaceProbe {
    pub repo_index: usize,
    pub path: String,
    pub planned_bytes: u64,
    pub checked_at: Instant,
    pub shortfall: Option<DiskSpaceShortfall>,
}

impl Foxy {
    /// Shortfall the update modal shows for `repo_index` before the user
    /// starts the download. Once the downloader has refused a run, its exact
    /// numbers replace the modal's estimate until the next attempt.
    pub(crate) fn update_modal_disk_space_shortfall(
        &mut self,
        repo_index: usize,
        estimated_bytes: u64,
    ) -> Option<DiskSpaceShortfall> {
        let (path, planned_bytes) = match &self.download_disk_space_shortfall {
            Some((refused_index, refused)) if *refused_index == repo_index => (
                refused.path.clone(),
                refused.needed_bytes.saturating_sub(DISK_SPACE_MARGIN_BYTES),
            ),
            _ => {
                let path = self
                    .repository_view_state
                    .repositories
                    .get(repo_index)
                    .map(|repo| repo.path.trim().to_string())?;
                (path, estimated_bytes)
            }
        };
        if path.is_empty() || planned_bytes == 0 {
            return None;
        }

        let cached = self.update_modal_disk_space_probe.as_ref().filter(|probe| {
            probe.repo_index == repo_index
                && probe.path == path
                && probe.planned_bytes == planned_bytes
                && probe.checked_at.elapsed() < PROBE_TTL
        });
        if let Some(probe) = cached {
            return probe.shortfall.clone();
        }

        let shortfall = probe_disk_space_shortfall(planned_bytes, Path::new(&path))
            .ok()
            .flatten();
        self.update_modal_disk_space_probe = Some(UpdateModalDiskSpaceProbe {
            repo_index,
            path,
            planned_bytes,
            checked_at: Instant::now(),
            shortfall: shortfall.clone(),
        });
        shortfall
    }

    pub(crate) fn disk_space_shortfall_title(&self, shortfall: &DiskSpaceShortfall) -> String {
        self.t_fmt(
            "Not enough free space on {path}",
            &[("path", shortfall.path.clone())],
        )
    }

    pub(crate) fn disk_space_shortfall_detail(&self, shortfall: &DiskSpaceShortfall) -> String {
        self.t_fmt(
            "The update needs {needed}, but only {available} is free. Free up at least {missing} on that drive, then start the download again.",
            &[
                ("needed", fmt_bytes(shortfall.needed_bytes)),
                ("available", fmt_bytes(shortfall.available_bytes)),
                ("missing", fmt_bytes(shortfall.missing_bytes())),
            ],
        )
    }

    /// One line for the banner behind the modal and the activity log.
    pub(crate) fn disk_space_shortfall_message(&self, shortfall: &DiskSpaceShortfall) -> String {
        format!(
            "{}. {}",
            self.disk_space_shortfall_title(shortfall),
            self.disk_space_shortfall_detail(shortfall)
        )
    }

    pub(crate) fn disk_space_shortfall_toast(
        &self,
        repo_name: &str,
        shortfall: &DiskSpaceShortfall,
    ) -> String {
        self.t_fmt(
            "Not enough free space to update {name}: free up at least {missing} on {path}.",
            &[
                ("name", repo_name.to_string()),
                ("missing", fmt_bytes(shortfall.missing_bytes())),
                ("path", shortfall.path.clone()),
            ],
        )
    }
}
