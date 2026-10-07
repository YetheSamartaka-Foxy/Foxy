use super::*;
use crate::ui::app::debug_modals::DebugModal;

/// How long a successful app update answer stays good.
const APP_UPDATE_RECHECK_INTERVAL: std::time::Duration =
    std::time::Duration::from_secs(6 * 60 * 60);
/// How soon a failed check is retried. Shorter than the success interval: a
/// failure means the answer is unknown, and unknown is the state worth leaving.
const APP_UPDATE_RETRY_INTERVAL: std::time::Duration = std::time::Duration::from_secs(15 * 60);

impl Foxy {
    pub(in crate::ui) fn app_update_source_configured(&self) -> bool {
        match self.settings_view_state.app_update_mode {
            crate::ui::types::AppUpdateMode::Server => {
                !self.settings_view_state.app_update_url.trim().is_empty()
            }
            crate::ui::types::AppUpdateMode::GitHub => {
                let repo = self.settings_view_state.app_update_github_repo.trim();
                !repo.is_empty() && repo.contains('/')
            }
        }
    }

    pub fn poll_app_update_events(&mut self) {
        // Collect events first to avoid borrow conflicts with &self.app_update_event_rx
        let events: Vec<_> = self
            .app_update_event_rx
            .as_ref()
            .map(|rx| {
                let mut evts = Vec::new();
                while let Ok(event) = rx.try_recv() {
                    evts.push(event);
                }
                evts
            })
            .unwrap_or_default();

        for event in events {
            match event {
                AppUpdateEvent::ManifestFetched(mut info) => {
                    log::info!(
                        "Update available: {} (current: {})",
                        info.manifest.latest,
                        info.current_version
                    );
                    // GitHub mode pre-populates changelogs; server mode fetches lazily
                    if !info.fetched_changelogs.is_empty() {
                        self.app_update_changelogs = std::mem::take(&mut info.fetched_changelogs);
                        self.app_update_changelogs_requested = true;
                    } else {
                        self.request_changelogs(&info.source_base_url, &info.manifest.versions);
                    }
                    self.app_update_status = UpdateCheckStatus::Available(info);
                    self.app_update_last_check = Some(std::time::Instant::now());
                    if std::mem::take(&mut self.app_update_prompt_armed) {
                        self.pending_app_update_prompt = true;
                    }
                    self.needs_repaint = true;
                }
                AppUpdateEvent::UpToDate(mut info) => {
                    log::info!("App is up to date.");
                    if !info.fetched_changelogs.is_empty() {
                        self.app_update_changelogs = std::mem::take(&mut info.fetched_changelogs);
                        self.app_update_changelogs_requested = true;
                    }
                    self.app_update_status = UpdateCheckStatus::UpToDate(info);
                    self.app_update_last_check = Some(std::time::Instant::now());
                    self.app_update_prompt_armed = false;
                    self.needs_repaint = true;
                }
                AppUpdateEvent::Error(msg) => {
                    log::warn!("Update check failed: {}", msg);
                    self.app_update_status = UpdateCheckStatus::Failed(msg);
                    // Record the attempt, not just the answer: the retry timer
                    // reads this, and without it a failed check would restart on
                    // the next frame.
                    self.app_update_last_check = Some(std::time::Instant::now());
                    self.app_update_prompt_armed = false;
                    self.needs_repaint = true;
                }
                _ => {}
            }
        }

        // Poll changelog fetch results (separate channel)
        if let Some(rx) = &self.app_update_changelog_rx {
            while let Ok(event) = rx.try_recv() {
                match event {
                    AppUpdateEvent::ChangelogFetched(cl) => {
                        self.app_update_changelog_loading.remove(&cl.version);
                        if !self
                            .app_update_changelogs
                            .iter()
                            .any(|c| c.version == cl.version)
                        {
                            self.app_update_changelogs.push(cl);
                        }
                        self.needs_repaint = true;
                    }
                    AppUpdateEvent::ChangelogFetchFailed(ver) => {
                        self.app_update_changelog_loading.remove(&ver);
                        self.needs_repaint = true;
                    }
                    _ => {}
                }
            }
        }

        // Poll download progress
        if let Some(rx) = &self.app_update_download_rx {
            while let Ok(event) = rx.try_recv() {
                match event {
                    AppUpdateEvent::DownloadProgress {
                        bytes_done,
                        bytes_total,
                    } => {
                        let progress = if bytes_total > 0 {
                            bytes_done as f32 / bytes_total as f32
                        } else {
                            0.0
                        };
                        self.app_update_status = UpdateCheckStatus::Downloading {
                            progress,
                            bytes_done,
                            bytes_total,
                        };
                        self.needs_repaint = true;
                    }
                    AppUpdateEvent::Verifying => {
                        self.app_update_status = UpdateCheckStatus::Verifying;
                        self.needs_repaint = true;
                    }
                    AppUpdateEvent::InstallerReady { installer_path } => {
                        log::info!(
                            "Installer ready at {}",
                            crate::core::utils::format::sanitize_log_path(&installer_path)
                        );
                        self.app_update_status =
                            UpdateCheckStatus::ReadyToInstall { installer_path };
                        self.needs_repaint = true;
                    }
                    AppUpdateEvent::Error(msg) => {
                        log::warn!("Download failed: {}", msg);
                        self.app_update_status = UpdateCheckStatus::Failed(msg);
                        self.needs_repaint = true;
                    }
                    _ => {}
                }
            }
        }
    }

    // -----------------------------------------------------------------------
    // Actions
    // -----------------------------------------------------------------------

    /// Re-check for an app update while the session is still running.
    ///
    /// The launch check is the only one Foxy used to perform, so a session left
    /// open across a release never learned about it. A background re-check costs
    /// one request against a manifest that is already fetched at every launch.
    pub(in crate::ui) fn maybe_recheck_app_update(&mut self) {
        if !self.settings_view_state.app_update_auto_check || !self.app_update_source_configured() {
            return;
        }
        // A real check would overwrite the seeded preview status, which is the
        // same reason the launch check skips it.
        if self.previewing_debug_modal(DebugModal::AppUpdate) {
            return;
        }
        // Never interrupt a check in flight, a download, or a staged installer:
        // restarting the check would discard state the user is about to act on.
        // `Available` stops the timer too - the answer is already on screen.
        let interval = match &self.app_update_status {
            UpdateCheckStatus::Idle | UpdateCheckStatus::UpToDate(_) => APP_UPDATE_RECHECK_INTERVAL,
            UpdateCheckStatus::Failed(_) => APP_UPDATE_RETRY_INTERVAL,
            _ => return,
        };
        if self
            .app_update_last_check
            .is_some_and(|last| last.elapsed() < interval)
        {
            return;
        }
        log::info!(
            "Re-checking for an app update: no answer in the last {} minutes",
            interval.as_secs() / 60
        );
        // Deliberately not arming the prompt. That modal is the launch prompt;
        // opening it over a session in progress interrupts whatever the user is
        // doing. The status still becomes `Available`, and the prompt returns on
        // the next launch.
        self.start_update_check();
    }

    pub fn start_update_check(&mut self) {
        let mode = self.settings_view_state.app_update_mode;
        let source = match mode {
            crate::ui::types::AppUpdateMode::Server => {
                let url = self.settings_view_state.app_update_url.trim().to_string();
                if url.is_empty() {
                    self.app_update_status =
                        UpdateCheckStatus::Failed("No update source URL configured.".to_string());
                    return;
                }
                url
            }
            crate::ui::types::AppUpdateMode::GitHub => {
                let repo = self
                    .settings_view_state
                    .app_update_github_repo
                    .trim()
                    .to_string();
                if repo.is_empty() || !repo.contains('/') {
                    self.app_update_status = UpdateCheckStatus::Failed(
                        "No GitHub repository configured. Enter as 'owner/repo'.".to_string(),
                    );
                    return;
                }
                repo
            }
        };
        let current_version = env!("CARGO_PKG_VERSION").to_string();
        self.app_update_status = UpdateCheckStatus::Checking;
        self.app_update_changelogs.clear();
        self.app_update_changelog_loading.clear();
        self.app_update_changelogs_requested = false;

        // Reset changelog channel
        let (cl_tx, cl_rx) = std::sync::mpsc::channel::<AppUpdateEvent>();
        self.app_update_changelog_tx = Some(cl_tx);
        self.app_update_changelog_rx = Some(cl_rx);

        let rx =
            app_update::spawn_update_check(mode, source, current_version, self.repaint_ctx.clone());
        self.app_update_event_rx = Some(rx);
    }

    pub fn start_installer_download(&mut self, version_entry: &VersionEntry) {
        if self.previewing_debug_modal(DebugModal::AppUpdate) {
            log::info!(
                "Debug modal preview: skipping installer download for v{}",
                version_entry.version
            );
            return;
        }
        let platform_key = app_update::current_platform_key();
        let Some(platform) = version_entry.platforms.get(platform_key) else {
            self.app_update_status = UpdateCheckStatus::Failed(format!(
                "No installer available for platform: {}",
                platform_key
            ));
            return;
        };

        let base_url = match self.settings_view_state.app_update_mode {
            crate::ui::types::AppUpdateMode::Server => {
                self.settings_view_state.app_update_url.trim().to_string()
            }
            crate::ui::types::AppUpdateMode::GitHub => {
                // GitHub mode stores full URLs in installer_path, base_url is unused
                String::new()
            }
        };
        self.app_update_status = UpdateCheckStatus::Downloading {
            progress: 0.0,
            bytes_done: 0,
            bytes_total: platform.installer_size,
        };

        let rx = app_update::spawn_installer_download(
            base_url,
            platform.clone(),
            self.repaint_ctx.clone(),
        );
        self.app_update_download_rx = Some(rx);
    }

    pub(super) fn request_changelogs(&mut self, base_url: &str, versions: &[VersionEntry]) {
        if self.app_update_changelogs_requested {
            return;
        }
        self.app_update_changelogs_requested = true;

        let Some(tx) = self.app_update_changelog_tx.clone() else {
            return;
        };

        for entry in versions {
            let ver = entry.version.clone();
            if self.app_update_changelog_loading.contains(&ver) {
                continue;
            }
            if self.app_update_changelogs.iter().any(|c| c.version == ver) {
                continue;
            }

            self.app_update_changelog_loading.insert(ver.clone());

            let base_url = base_url.to_string();
            let changelog_path = entry.changelog.clone();
            let tx = tx.clone();
            let repaint_ctx = self.repaint_ctx.clone();

            std::thread::spawn(move || {
                let rt = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(rt) => rt,
                    Err(e) => {
                        log::error!("Failed to create tokio runtime for changelog fetch: {}", e);
                        let _ = tx.send(AppUpdateEvent::ChangelogFetchFailed(ver));
                        return;
                    }
                };
                rt.block_on(async {
                    match app_update::fetch_changelog(&base_url, &changelog_path).await {
                        Ok(cl) => {
                            let _ = tx.send(AppUpdateEvent::ChangelogFetched(cl));
                        }
                        Err(e) => {
                            log::warn!("Failed to fetch changelog for {}: {}", ver, e);
                            let _ = tx.send(AppUpdateEvent::ChangelogFetchFailed(ver));
                        }
                    }
                });
                if let Some(ctx) = repaint_ctx {
                    ctx.request_repaint();
                }
            });
        }
    }

    pub(super) fn fetch_single_changelog(
        &mut self,
        base_url: &str,
        changelog_path: &str,
        version: &str,
    ) {
        if self.app_update_changelog_loading.contains(version) {
            return;
        }
        if self
            .app_update_changelogs
            .iter()
            .any(|c| c.version == version)
        {
            return;
        }

        let Some(tx) = self.app_update_changelog_tx.clone() else {
            return;
        };

        self.app_update_changelog_loading
            .insert(version.to_string());
        let base_url = base_url.to_string();
        let changelog_path = changelog_path.to_string();
        let repaint_ctx = self.repaint_ctx.clone();
        let version_owned = version.to_string();

        std::thread::spawn(move || {
            let rt = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    log::error!("Failed to create tokio runtime for changelog fetch: {}", e);
                    let _ = tx.send(AppUpdateEvent::ChangelogFetchFailed(version_owned));
                    return;
                }
            };
            rt.block_on(async {
                match app_update::fetch_changelog(&base_url, &changelog_path).await {
                    Ok(cl) => {
                        let _ = tx.send(AppUpdateEvent::ChangelogFetched(cl));
                    }
                    Err(e) => {
                        log::warn!("Failed to fetch changelog for {}: {}", version_owned, e);
                        let _ = tx.send(AppUpdateEvent::ChangelogFetchFailed(version_owned));
                    }
                }
            });
            if let Some(ctx) = repaint_ctx {
                ctx.request_repaint();
            }
        });
    }

    // -----------------------------------------------------------------------
    // Shared: download/install status rendering (used by both views)
    // -----------------------------------------------------------------------
}
