use super::PROFILE_CLIPBOARD_HEADER;
use crate::ui::app::Foxy;
use crate::ui::types::{Repository, RepositoryProfile};
use arboard::Clipboard;
use log::{info, warn};

fn detect_display_server() -> &'static str {
    #[cfg(target_os = "linux")]
    {
        if std::env::var("WAYLAND_DISPLAY").is_ok() {
            return "Wayland";
        }
        if std::env::var("DISPLAY").is_ok() {
            return "X11";
        }
        "unknown"
    }
    #[cfg(not(target_os = "linux"))]
    {
        "native"
    }
}

impl Foxy {
    pub(super) fn export_profile_to_clipboard(&mut self, repo_index: usize) {
        let profile = {
            let Some(repo) = self.repository_view_state.repositories.get(repo_index) else {
                warn!("Export profile failed: repository index out of bounds.");
                self.show_error_toast(self.t("Failed to export profile."));
                return;
            };

            if let Some(selected) = &repo.selected_profile {
                repo.profiles.iter().find(|p| &p.name == selected).cloned()
            } else {
                Some(Self::profile_from_repository(repo, "Default".to_string()))
            }
        };

        let profile = match profile {
            Some(profile) => profile,
            None => {
                warn!("Export profile skipped: selected profile not found.");
                self.show_error_toast(self.t("Failed to export profile."));
                return;
            }
        };

        let json = match serde_json::to_string_pretty(&profile) {
            Ok(json) => json,
            Err(err) => {
                warn!("Export profile failed to serialize: {}", err);
                self.show_error_toast(self.t("Failed to export profile."));
                return;
            }
        };

        let data = format!("{}\n{}", PROFILE_CLIPBOARD_HEADER, json);

        match Clipboard::new().and_then(|mut clipboard| clipboard.set_text(data)) {
            Ok(_) => {
                info!("Profile exported to clipboard.");
                self.show_success_toast(self.t("Profile exported to clipboard."));
            }
            Err(err) => {
                let display_server = detect_display_server();
                warn!(
                    "Failed to copy profile to clipboard (display server: {}): {}",
                    display_server, err
                );
                self.show_error_toast(self.t("Failed to copy profile to clipboard."));
            }
        }
    }

    pub(super) fn import_profile_from_clipboard(&mut self, repo_index: usize) {
        let clipboard_text = match Clipboard::new().and_then(|mut clipboard| clipboard.get_text()) {
            Ok(text) => text,
            Err(err) => {
                let display_server = detect_display_server();
                warn!(
                    "Failed to read clipboard (display server: {}): {}",
                    display_server, err
                );
                self.show_error_toast(self.t("Failed to read clipboard."));
                return;
            }
        };

        if !clipboard_text.starts_with(PROFILE_CLIPBOARD_HEADER) {
            warn!("Clipboard does not contain an exported profile.");
            self.show_error_toast(self.t("Clipboard does not contain an exported profile."));
            return;
        }

        let payload = clipboard_text[PROFILE_CLIPBOARD_HEADER.len()..].trim_start();
        let mut profile: RepositoryProfile = match serde_json::from_str(payload) {
            Ok(profile) => profile,
            Err(err) => {
                warn!("Failed to parse profile data: {}", err);
                self.show_error_toast(self.t("Failed to import profile from clipboard."));
                return;
            }
        };

        let base_name = if profile.name.trim().is_empty() {
            "Imported Profile".to_string()
        } else {
            profile.name.trim().to_string()
        };

        if repo_index >= self.repository_view_state.repositories.len() {
            warn!("Import profile failed: repository index out of bounds.");
            self.show_error_toast(self.t("Failed to import profile from clipboard."));
            return;
        }

        let repo = &mut self.repository_view_state.repositories[repo_index];
        let unique_name = Self::unique_profile_name(repo, &base_name, "_copy");
        profile.name = unique_name.clone();
        repo.profiles.push(profile);
        repo.selected_profile = Some(unique_name);

        self.save_repositories();
        info!("Profile imported from clipboard.");
        self.show_success_toast(self.t("Profile imported from clipboard."));
    }

    pub(super) fn profile_from_repository(repo: &Repository, name: String) -> RepositoryProfile {
        RepositoryProfile {
            name,
            csla: repo.csla,
            ef: repo.ef,
            gm: repo.gm,
            rf: repo.rf,
            spe: repo.spe,
            vn: repo.vn,
            ws: repo.ws,
            skip_intro: repo.skip_intro,
            no_splash: repo.no_splash,
            world_empty: repo.world_empty,
            load_mission_to_memory: repo.load_mission_to_memory,
            enable_ht: repo.enable_ht,
            huge_pages: repo.huge_pages,
            no_logs: repo.no_logs,
            include_steam_addons: repo.include_steam_addons,
            additional_params: repo.additional_params.clone(),
            addons: repo.addons.clone(),
            optional_addons: repo.optional_addons.clone(),
            optional_addon_favorites: repo.optional_addon_favorites.clone(),
            optional_addon_client_side: repo.optional_addon_client_side.clone(),
            external_addons: repo.external_addons.clone(),
            external_addon_favorites: repo.external_addon_favorites.clone(),
            external_addon_client_side: repo.external_addon_client_side.clone(),
        }
    }

    pub(super) fn unique_profile_name(repo: &Repository, base: &str, sep: &str) -> String {
        if !repo.profiles.iter().any(|p| p.name == base) {
            return base.to_string();
        }

        let mut count = 1;
        loop {
            let candidate = format!("{}{}{}", base, sep, count);
            if !repo.profiles.iter().any(|p| p.name == candidate) {
                return candidate;
            }
            count += 1;
        }
    }
}
