use crate::core::api::SyncMode;
use crate::ui::app::{Foxy, RepositoryCheckCompletionState};
use std::time::Duration;

impl Foxy {
    pub(super) fn repository_check_completion_title(
        &self,
        banner: &RepositoryCheckCompletionState,
    ) -> String {
        match (banner.mode, banner.success) {
            (SyncMode::QuickCheckOnly, true) => banner
                .elapsed
                .map(Self::format_compact_elapsed_duration)
                .map(|duration| {
                    self.t_fmt(
                        "Quick local check finished in {duration}",
                        &[("duration", duration)],
                    )
                })
                .unwrap_or_else(|| self.t("Quick local check finished")),
            (SyncMode::RemoteRefreshOnly, true) => banner
                .elapsed
                .map(Self::format_compact_elapsed_duration)
                .map(|duration| {
                    self.t_fmt(
                        "Remote data recheck finished in {duration}",
                        &[("duration", duration)],
                    )
                })
                .unwrap_or_else(|| self.t("Remote data recheck finished")),
            (SyncMode::RecheckOnly, true) => self.t("Repository recheck finished"),
            (SyncMode::RecheckIntegrity, true) => banner
                .elapsed
                .map(Self::format_compact_elapsed_duration)
                .map(|duration| {
                    self.t_fmt(
                        "Integrity recheck finished in {duration}",
                        &[("duration", duration)],
                    )
                })
                .unwrap_or_else(|| self.t("Integrity recheck finished")),
            (SyncMode::QuickCheckOnly, false) => self.t("Quick local check failed"),
            (SyncMode::RemoteRefreshOnly, false) => self.t("Remote data recheck failed"),
            (SyncMode::RecheckOnly, false) => self.t("Repository recheck failed"),
            (SyncMode::RecheckIntegrity, false) => self.t("Integrity recheck failed"),
            (SyncMode::Download, true) => self.t("Done"),
            (SyncMode::Download, false) => self.t("Update failed"),
        }
    }

    pub(super) fn repository_check_cycle_message(
        &self,
        mode: SyncMode,
        elapsed: Duration,
    ) -> String {
        let cycle = ((elapsed.as_millis() / 1400) % 4) as usize;
        let key = match mode {
            SyncMode::QuickCheckOnly => match cycle {
                0 => "Checking addon folders",
                1 => "Comparing local content hashes",
                2 => "Looking for changed files",
                _ => "Refreshing update summary",
            },
            SyncMode::RemoteRefreshOnly | SyncMode::RecheckOnly => match cycle {
                0 => "Fetching repository metadata",
                1 => "Comparing remote and local data",
                2 => "Refreshing local hash baseline",
                _ => "Preparing update list",
            },
            SyncMode::RecheckIntegrity => match cycle {
                0 => "Fetching repository metadata",
                1 => "Recalculating file hashes",
                2 => "Updating hash records",
                _ => "Refreshing update summary",
            },
            SyncMode::Download => "Updating...",
        };

        self.t(key)
    }

    pub(super) fn active_repository_download_stage_detail(&self) -> String {
        if self.download_paused {
            return self.t("Download paused");
        }

        self.download_progress
            .as_ref()
            .map(|(label, _)| self.translate_repository_download_stage(label))
            .unwrap_or_else(|| self.t("Preparing"))
    }

    pub(super) fn translate_repository_download_stage(&self, stage: &str) -> String {
        if stage == "Preparing" {
            self.t("Preparing")
        } else if stage == "Quick local verify" {
            self.t("Quick local verify")
        } else if stage == "Hashing..." {
            self.t("Calculating file hashes")
        } else if stage == "Hashing profile" {
            self.t("Hashing profile")
        } else if stage == "Done" {
            self.t("Done")
        } else if let Some(duration) = stage
            .strip_prefix("Download ")
            .and_then(|s| s.strip_suffix('s'))
        {
            self.t_fmt(
                "Download stage: {duration}",
                &[("duration", duration.to_string())],
            )
        } else if let Some(progress) = stage.strip_prefix("Download ") {
            self.t_fmt("Downloading {name}", &[("name", progress.to_string())])
        } else if let Some(duration) = stage
            .strip_prefix("Hash ")
            .and_then(|s| s.strip_suffix('s'))
        {
            self.t_fmt(
                "Hash stage: {duration}",
                &[("duration", duration.to_string())],
            )
        } else {
            stage.to_owned()
        }
    }

    pub(super) fn translate_repository_check_stage(&self, stage: &str) -> String {
        if stage == "Preparing" {
            self.t("Preparing")
        } else if stage == "Optimizing database" {
            self.t("Optimizing database")
        } else if stage == "Starting remote data recheck" {
            self.t("Starting remote data recheck")
        } else if stage == "Starting quick local check" {
            self.t("Starting quick local check")
        } else if stage == "Starting repository recheck" {
            self.t("Starting repository recheck")
        } else if stage == "Starting integrity recheck" {
            self.t("Starting integrity recheck")
        } else if stage == "Quick local check" {
            self.t("Quick local check")
        } else if stage == "Quick addon content hash check" {
            self.t("Quick addon content hash check")
        } else if stage == "Calculating file hashes" {
            self.t("Calculating file hashes")
        } else if stage == "Recalculating file hashes" {
            self.t("Recalculating file hashes")
        } else if stage == "Hashing profile" {
            self.t("Hashing profile")
        } else if stage == "Building update status" {
            self.t("Building update status")
        } else if stage == "Refreshing content-hash baseline" {
            self.t("Refreshing content-hash baseline")
        } else if stage == "Initializing tree hashes" {
            self.t("Initializing tree hashes")
        } else if stage == "Initializing local tree hashes" {
            self.t("Initializing local tree hashes")
        } else if stage == "Cleaning unexpected local files" {
            self.t("Cleaning unexpected local files")
        } else if stage == "Refreshing content hashes" {
            self.t("Refreshing content hashes")
        } else if stage == "Propagating checksums to sibling repositories" {
            self.t("Propagating checksums to sibling repositories")
        } else if stage == "Recheck completed" {
            self.t("Recheck completed")
        } else if stage == "Done" {
            self.t("Done")
        } else if stage == "Updating repositories" {
            self.t("Updating repositories")
        } else if let Some(count) =
            Self::parse_stage_count(stage, "Initializing missing tree hashes (", ")")
        {
            self.t_fmt(
                "Initializing missing tree hashes ({count})",
                &[("count", count.to_string())],
            )
        } else if let Some(count) =
            Self::parse_stage_count(stage, "Tree hash verify recommended for ", " addons")
        {
            self.i18n.tr_plural(
                "Tree hash verify recommended for {count} addons",
                count as u64,
            )
        } else if let Some(count) =
            Self::parse_stage_count(stage, "Verifying tree hashes for ", " files")
        {
            self.i18n
                .tr_plural("Verifying tree hashes for {count} files", count as u64)
        } else if let Some((checked, total)) =
            Self::parse_stage_progress_pair(stage, "Hashing ", " files")
        {
            self.t_fmt(
                "Hashing {checked}/{total} files",
                &[
                    ("checked", checked.to_string()),
                    ("total", total.to_string()),
                ],
            )
        } else if let Some((checked, total)) =
            Self::parse_stage_progress_pair(stage, "Saving parts ", "")
        {
            self.t_fmt(
                "Saving parts {checked}/{total}",
                &[
                    ("checked", checked.to_string()),
                    ("total", total.to_string()),
                ],
            )
        } else if let Some((checked, total)) =
            Self::parse_stage_progress_pair(stage, "Updating files ", "")
        {
            self.t_fmt(
                "Updating files {checked}/{total}",
                &[
                    ("checked", checked.to_string()),
                    ("total", total.to_string()),
                ],
            )
        } else if let Some((checked, total)) =
            Self::parse_stage_progress_pair(stage, "Saving files ", "")
        {
            self.t_fmt(
                "Saving files {checked}/{total}",
                &[
                    ("checked", checked.to_string()),
                    ("total", total.to_string()),
                ],
            )
        } else if let Some((checked, total)) =
            Self::parse_stage_progress_pair(stage, "Updating addons ", "")
        {
            self.t_fmt(
                "Updating addons {checked}/{total}",
                &[
                    ("checked", checked.to_string()),
                    ("total", total.to_string()),
                ],
            )
        } else if let Some((checked, total)) =
            Self::parse_stage_progress_pair(stage, "Saving addons ", "")
        {
            self.t_fmt(
                "Saving addons {checked}/{total}",
                &[
                    ("checked", checked.to_string()),
                    ("total", total.to_string()),
                ],
            )
        } else if let Some((checked, total)) =
            Self::parse_stage_progress_pair(stage, "Updating repositories ", "")
        {
            self.t_fmt(
                "Updating repositories {checked}/{total}",
                &[
                    ("checked", checked.to_string()),
                    ("total", total.to_string()),
                ],
            )
        } else if let Some((checked, total)) =
            Self::parse_stage_progress_pair(stage, "Saving repositories ", "")
        {
            self.t_fmt(
                "Saving repositories {checked}/{total}",
                &[
                    ("checked", checked.to_string()),
                    ("total", total.to_string()),
                ],
            )
        } else {
            stage.to_owned()
        }
    }

    pub(super) fn parse_stage_count(label: &str, prefix: &str, suffix: &str) -> Option<usize> {
        let raw = label.strip_prefix(prefix)?.strip_suffix(suffix)?;
        raw.trim().parse().ok()
    }

    pub(super) fn parse_stage_progress_pair(
        label: &str,
        prefix: &str,
        suffix: &str,
    ) -> Option<(usize, usize)> {
        let raw = label.strip_prefix(prefix)?.strip_suffix(suffix)?;
        let (checked, total) = raw.split_once('/')?;
        Some((checked.trim().parse().ok()?, total.trim().parse().ok()?))
    }
}
