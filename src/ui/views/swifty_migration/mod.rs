pub mod scanner;
pub mod types;

use crate::ui::app::{Foxy, RepositorySpaceImportContinuation};
use crate::ui::i18n::{tr, tr_fmt};
use eframe::egui::{self, Button, Label, Margin, RichText, ScrollArea, TextEdit, Ui, Vec2};
use log::info;

use scanner::derive_urls;
use types::SwiftyDetectedRepo;

use crate::ui::types::{Repository, RepositoryProfile};

impl Foxy {
    fn profile_from_swifty_repo(
        swifty_repo: &SwiftyDetectedRepo,
        name: String,
    ) -> RepositoryProfile {
        let mut repo = Repository::default();
        if !swifty_repo.parameters.trim().is_empty() {
            scanner::apply_swifty_parameters(&mut repo, &swifty_repo.parameters);
        }

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
            additional_params: repo.additional_params,
            addons: repo.addons,
            optional_addons: repo.optional_addons,
            optional_addon_favorites: Vec::new(),
            optional_addon_client_side: Vec::new(),
            external_addons: repo.external_addons,
            external_addon_favorites: Vec::new(),
            external_addon_client_side: Vec::new(),
        }
    }

    fn unique_swifty_profile_name(repo: &Repository, swifty_name: &str) -> String {
        let base_name = swifty_name.trim();
        let base_name = if base_name.is_empty() {
            RepositoryProfile::default().name
        } else {
            base_name.to_string()
        };

        if !repo.profiles.iter().any(|p| p.name == base_name) {
            return base_name;
        }

        let mut count = 1;
        loop {
            let candidate = format!("{} {}", base_name, count);
            if !repo.profiles.iter().any(|p| p.name == candidate) {
                return candidate;
            }
            count += 1;
        }
    }

    fn add_swifty_repo_as_profile(repo: &mut Repository, swifty_repo: &SwiftyDetectedRepo) {
        let profile_name = Self::unique_swifty_profile_name(repo, &swifty_repo.name);
        let profile = Self::profile_from_swifty_repo(swifty_repo, profile_name.clone());
        repo.profiles.push(profile);
        info!(
            "Imported Swifty repo '{}' as profile '{}' for repository '{}'",
            swifty_repo.name, profile_name, repo.name
        );
    }

    fn swifty_space_binding_for_repo(
        &self,
        space_id: &str,
        repo_address: &str,
    ) -> Option<(String, String)> {
        let space = self.repository_spaces.iter().find(|s| s.id == space_id)?;
        let normalized_repo = Self::normalize_repo_url(repo_address);
        if let Some(entry) = space
            .entries
            .iter()
            .find(|e| Self::normalize_repo_url(&e.address) == normalized_repo)
        {
            return Some((entry.address.clone(), space.shared_path.clone()));
        }

        let urls = derive_urls(repo_address)?;
        let space_base = Self::normalize_repo_url(&space.source_base_url);
        let repo_base = Self::normalize_repo_url(urls.base_url.trim_end_matches('/'));
        (space_base == repo_base).then(|| (repo_address.to_string(), space.shared_path.clone()))
    }

    /// Open the Swifty migration view and trigger scanning.
    pub fn open_swifty_migration_view(&mut self) {
        self.last_view = self.current_view;
        self.current_view = crate::ui::types::FoxyView::SwiftyMigration;
        self.ensure_swifty_migration_scanned();
    }

    /// Lazily scan Swifty data if not already done.
    pub fn ensure_swifty_migration_scanned(&mut self) {
        if self.swifty_migration_state.scan_complete {
            return;
        }
        let (repos, global, error) = scanner::scan_swifty_repositories();

        // Auto-detect updater URL and repository-space URL from the first repo that yields one.
        for repo in &repos {
            if let Some(urls) = derive_urls(&repo.address) {
                if self.swifty_migration_state.detected_updater_url.is_empty() {
                    self.swifty_migration_state.detected_updater_url = urls.updater_url;
                }
                if self.swifty_migration_state.detected_space_url.is_empty() {
                    self.swifty_migration_state.detected_space_url = urls.space_url;
                }
                break;
            }
        }

        self.swifty_migration_state.global_settings = global;
        self.swifty_migration_state.detected_repos = repos;
        self.swifty_migration_state.scan_error = error;
        self.swifty_migration_state.scan_complete = true;
    }

    /// Main render entry point for the migration view.
    pub fn render_swifty_migration_view(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let settings_margin = Margin {
            left: 15,
            right: 15,
            top: 10,
            bottom: 10,
        };

        egui::Frame::NONE
            .inner_margin(settings_margin)
            .show(ui, |ui| {
                ui.vertical(|ui| {
                    self.render_migration_header(ui);
                    ui.separator();
                    self.render_migration_body(ui);
                });
            });
    }

    fn render_migration_header(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            ui.heading(
                RichText::new(tr("Migrate from Swifty"))
                    .size(self.settings_view_state.font_sizes.settings_view.page_title as f32),
            );

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let close_icon_size =
                    self.settings_view_state.font_sizes.settings_view.close_icon as f32;
                let close_button = ui.add_sized(
                    Self::modal_icon_button_size(close_icon_size),
                    Button::new(
                        RichText::new("X")
                            .color(self.color_text_normal())
                            .size(close_icon_size),
                    )
                    .fill(self.color_main_bg()),
                );
                if close_button.hovered() {
                    ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                }
                if close_button.clicked() {
                    info!("Closing Swifty migration view");
                    self.settings_view_state.swifty_migration_offered = true;
                    self.save_settings();
                    self.restore_last_view_or_default();
                }
            });
        });
    }

    fn render_migration_body(&mut self, ui: &mut Ui) {
        let horizontal_padding = 15.0;

        // Info banner
        ui.horizontal(|ui| {
            ui.add_space(horizontal_padding);
            let width = (ui.available_width() - horizontal_padding).max(0.0);
            ui.add_sized(
                Vec2::new(width, 0.0),
                Label::new(
                    RichText::new(format!(
                        "{} {}",
                        '\u{2139}',
                        tr("This wizard helps you import repositories from an existing Swifty installation. Your Swifty data will not be modified.")
                    ))
                    .italics()
                    .color(self.color_text_dim()),
                )
                .wrap(),
            );
        });
        ui.separator();

        // Error or no-results message
        if let Some(error) = self.swifty_migration_state.scan_error.clone() {
            ui.horizontal(|ui| {
                ui.add_space(horizontal_padding);
                ui.label(RichText::new(error).color(self.color_text_dim()));
            });
            ui.add_space(10.0);
            self.render_migration_close_button(ui, horizontal_padding);
            return;
        }

        if self.swifty_migration_state.detected_repos.is_empty() {
            ui.horizontal(|ui| {
                ui.add_space(horizontal_padding);
                ui.label(
                    RichText::new(tr("No Swifty repositories found.")).color(self.color_text_dim()),
                );
            });
            ui.add_space(10.0);
            self.render_migration_close_button(ui, horizontal_padding);
            return;
        }

        // Import-done summary
        if self.swifty_migration_state.import_done {
            self.render_migration_import_done(ui, horizontal_padding);
            return;
        }

        // Repository list with checkboxes
        self.render_migration_repo_list(ui, horizontal_padding);
    }

    fn render_migration_repo_list(&mut self, ui: &mut Ui, horizontal_padding: f32) {
        let selected_count = self
            .swifty_migration_state
            .detected_repos
            .iter()
            .filter(|r| r.selected)
            .count();
        let total_count = self.swifty_migration_state.detected_repos.len();

        // --- Detected server settings (updater URL + repository space) ---
        self.render_migration_server_settings(ui, horizontal_padding);
        ui.add_space(4.0);
        ui.separator();
        ui.add_space(4.0);

        ui.horizontal(|ui| {
            ui.add_space(horizontal_padding);
            ui.label(tr_fmt(
                "Found {count} Swifty repositories:",
                &[("count", total_count.to_string())],
            ));
        });
        ui.add_space(4.0);

        // Select/deselect all
        ui.horizontal(|ui| {
            ui.add_space(horizontal_padding);
            let select_all_btn = ui.add(Button::new(tr("Select all")).fill(self.color_widget_bg()));
            if select_all_btn.hovered() {
                ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
            }
            if select_all_btn.clicked() {
                for repo in &mut self.swifty_migration_state.detected_repos {
                    repo.selected = true;
                }
            }

            let deselect_all_btn =
                ui.add(Button::new(tr("Deselect all")).fill(self.color_widget_bg()));
            if deselect_all_btn.hovered() {
                ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
            }
            if deselect_all_btn.clicked() {
                for repo in &mut self.swifty_migration_state.detected_repos {
                    repo.selected = false;
                }
            }
        });
        ui.add_space(4.0);

        ScrollArea::vertical()
            .max_height(ui.available_height() - 60.0)
            .show(ui, |ui| {
                for i in 0..self.swifty_migration_state.detected_repos.len() {
                    self.render_migration_repo_card(ui, i, horizontal_padding);
                    ui.add_space(6.0);
                }
            });

        ui.add_space(6.0);

        // Import button
        let ctx = ui.ctx().clone();
        ui.horizontal(|ui| {
            ui.add_space(horizontal_padding);
            let import_label = tr_fmt(
                "Import {count} selected repositories",
                &[("count", selected_count.to_string())],
            );
            let import_btn = ui.add_sized(
                Vec2::new(ui.available_width() - 2.0 * horizontal_padding, 32.0),
                Button::new(import_label).fill(if selected_count > 0 {
                    self.color_widget_bg()
                } else {
                    self.color_card_bg()
                }),
            );
            if import_btn.hovered() && selected_count > 0 {
                ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
            }
            if import_btn.clicked() && selected_count > 0 {
                self.execute_swifty_import(&ctx);
            }
            ui.add_space(horizontal_padding);
        });
    }

    /// Render compact inline fields for server URLs and global paths.
    fn render_migration_server_settings(&mut self, ui: &mut Ui, horizontal_padding: f32) {
        let label_width = 170.0;

        ui.horizontal(|ui| {
            ui.add_space(horizontal_padding);
            ui.add_sized(
                Vec2::new(label_width, 20.0),
                Label::new(
                    RichText::new(tr("Update server URL"))
                        .color(self.color_text_normal())
                        .strong(),
                ),
            );
            ui.add_sized(
                Vec2::new(ui.available_width() - horizontal_padding, 20.0),
                TextEdit::singleline(&mut self.swifty_migration_state.detected_updater_url)
                    .hint_text(tr("e.g. http://your-server.com/mods/Foxy")),
            );
        });

        ui.add_space(2.0);

        ui.horizontal(|ui| {
            ui.add_space(horizontal_padding);
            ui.add_sized(
                Vec2::new(label_width, 20.0),
                Label::new(
                    RichText::new(tr("Repository space URL"))
                        .color(self.color_text_normal())
                        .strong(),
                ),
            );
            ui.add_sized(
                Vec2::new(ui.available_width() - horizontal_padding, 20.0),
                TextEdit::singleline(&mut self.swifty_migration_state.detected_space_url)
                    .hint_text(tr("e.g. http://your-server.com/mods/repository_space.json")),
            );
        });

        // --- Detected global paths ---
        let has_arma = !self
            .swifty_migration_state
            .global_settings
            .arma_path
            .trim()
            .is_empty();
        let has_temp = !self
            .swifty_migration_state
            .global_settings
            .temp_path
            .trim()
            .is_empty();

        if has_arma || has_temp {
            ui.add_space(2.0);

            if has_arma {
                ui.horizontal(|ui| {
                    ui.add_space(horizontal_padding);
                    ui.add_sized(
                        Vec2::new(label_width, 20.0),
                        Label::new(
                            RichText::new(tr("Arma 3 directory"))
                                .color(self.color_text_normal())
                                .strong(),
                        ),
                    );
                    ui.add_sized(
                        Vec2::new(ui.available_width() - horizontal_padding, 20.0),
                        TextEdit::singleline(
                            &mut self.swifty_migration_state.global_settings.arma_path,
                        ),
                    );
                });
                ui.add_space(2.0);
            }

            if has_temp {
                ui.horizontal(|ui| {
                    ui.add_space(horizontal_padding);
                    ui.add_sized(
                        Vec2::new(label_width, 20.0),
                        Label::new(
                            RichText::new(tr("Temp directory"))
                                .color(self.color_text_normal())
                                .strong(),
                        ),
                    );
                    ui.add_sized(
                        Vec2::new(ui.available_width() - horizontal_padding, 20.0),
                        TextEdit::singleline(
                            &mut self.swifty_migration_state.global_settings.temp_path,
                        ),
                    );
                });
            }
        }
    }

    fn render_migration_repo_card(&mut self, ui: &mut Ui, index: usize, _horizontal_padding: f32) {
        let repo = &self.swifty_migration_state.detected_repos[index];
        let name = repo.name.clone();
        let address = repo.address.clone();
        let mod_folder = repo.mod_folder.clone();
        let derived = derive_urls(&address);
        let mut selected = repo.selected;

        let card_frame = egui::Frame {
            fill: self.color_card_bg(),
            stroke: egui::Stroke::new(1.0, self.color_text_gray()),
            corner_radius: eframe::egui::CornerRadius::same(5),
            inner_margin: Margin::same(8),
            outer_margin: Margin {
                left: 15,
                right: 15,
                top: 0,
                bottom: 0,
            },
            ..Default::default()
        };

        card_frame.show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                let checkbox = Self::ui_state_checkbox(ui, &mut selected, "");
                if checkbox.hovered() {
                    ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                }

                ui.vertical(|ui| {
                    ui.label(
                        RichText::new(&name)
                            .color(self.color_text_normal())
                            .strong(),
                    );

                    ui.label(
                        RichText::new(tr_fmt(
                            "Address: {address}",
                            &[("address", address.clone())],
                        ))
                        .color(self.color_text_dim()),
                    );

                    if !mod_folder.is_empty() {
                        ui.label(
                            RichText::new(tr_fmt("Mod folder: {path}", &[("path", mod_folder)]))
                                .color(self.color_text_dim()),
                        );
                    }

                    if let Some(urls) = &derived {
                        ui.label(
                            RichText::new(tr_fmt(
                                "Derived base URL: {url}",
                                &[("url", urls.base_url.clone())],
                            ))
                            .color(self.color_text_dim()),
                        );
                    }
                });
            });
        });

        self.swifty_migration_state.detected_repos[index].selected = selected;
    }

    fn render_migration_import_done(&mut self, ui: &mut Ui, horizontal_padding: f32) {
        let count = self.swifty_migration_state.imported_count;
        ui.horizontal(|ui| {
            ui.add_space(horizontal_padding);
            ui.label(
                RichText::new(tr_fmt(
                    "Successfully imported {count} repositories from Swifty.",
                    &[("count", count.to_string())],
                ))
                .color(self.color_text_normal()),
            );
        });

        if self.swifty_migration_state.space_import_failed
            && !self
                .swifty_migration_state
                .detected_space_url
                .trim()
                .is_empty()
        {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.add_space(horizontal_padding);
                ui.label(
                    RichText::new(tr(
                        "Repository space could not be imported. You can add it manually later in settings.",
                    ))
                    .color(self.color_text_dim()),
                );
            });
        }

        ui.add_space(10.0);
        self.render_migration_close_button(ui, horizontal_padding);
    }

    fn render_migration_close_button(&mut self, ui: &mut Ui, horizontal_padding: f32) {
        ui.horizontal(|ui| {
            ui.add_space(horizontal_padding);
            let close_btn = ui.add_sized(
                Vec2::new(ui.available_width() - 2.0 * horizontal_padding, 32.0),
                Button::new(tr("Close")).fill(self.color_widget_bg()),
            );
            if close_btn.hovered() {
                ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
            }
            if close_btn.clicked() {
                self.settings_view_state.swifty_migration_offered = true;
                self.save_settings();
                self.restore_last_view_or_default();
            }
            ui.add_space(horizontal_padding);
        });
    }

    /// Begin the import: snapshot the selected Swifty repos and probe for a
    /// repository space. The manifest fetch runs off the UI thread, so the rest
    /// of the import is completed by [`Self::finish_swifty_import`] once the
    /// fetch resolves (via `poll_repository_space_import_results`).
    fn execute_swifty_import(&mut self, ctx: &egui::Context) {
        let selected: Vec<SwiftyDetectedRepo> = self
            .swifty_migration_state
            .detected_repos
            .iter()
            .filter(|r| r.selected)
            .cloned()
            .collect();

        // --- 1. Try to import the repository space first so we can bind repos
        // to it. With no space URL there is nothing to fetch, so finish now;
        // otherwise dispatch the fetch and continue once it returns. ---
        let space_url = self
            .swifty_migration_state
            .detected_space_url
            .trim()
            .to_string();
        if space_url.is_empty() {
            self.finish_swifty_import(selected, None, ctx);
            return;
        }
        if self.repository_space_import_in_flight {
            info!("Repository space import already in flight; ignoring duplicate migration import");
            return;
        }
        info!("Migration: fetching repository space from {}", space_url);
        self.dispatch_repository_space_import(
            &space_url,
            RepositorySpaceImportContinuation::SwiftyMigration { selected },
        );
    }

    /// Complete a Swifty migration import once the repository space (if any) has
    /// been fetched and applied: create Foxy `Repository` entries from the
    /// selected Swifty repos, bind them to the imported space, and migrate the
    /// updater URL and global settings.
    pub(crate) fn finish_swifty_import(
        &mut self,
        selected: Vec<SwiftyDetectedRepo>,
        imported_space_id: Option<String>,
        ctx: &egui::Context,
    ) {
        // --- 1b. Set the space shared_path from Swifty mod_folder paths. ---
        // Pick the most common non-empty mod_folder among selected repos.
        if let Some(ref space_id) = imported_space_id {
            let space_has_path = self
                .repository_spaces
                .iter()
                .find(|s| s.id == *space_id)
                .map(|s| !s.shared_path.trim().is_empty())
                .unwrap_or(false);

            if !space_has_path {
                let mut folder_counts: std::collections::HashMap<String, usize> =
                    std::collections::HashMap::new();
                for repo in &selected {
                    let folder = repo.mod_folder.trim();
                    if !folder.is_empty() {
                        *folder_counts.entry(folder.to_string()).or_insert(0) += 1;
                    }
                }
                if let Some((best_path, _)) = folder_counts.into_iter().max_by_key(|(_, c)| *c) {
                    info!(
                        "Migration: setting space shared path from Swifty mod folders: {}",
                        best_path
                    );
                    self.set_repository_space_shared_path(space_id, best_path);
                }
            }
        }

        // --- 2. Import individual repositories. ---
        let mut imported = 0usize;
        let mut imported_profiles = 0usize;
        let mut existing_to_refresh: Vec<usize> = Vec::new();
        let mut repo_index_by_address: std::collections::HashMap<String, usize> = self
            .repository_view_state
            .repositories
            .iter()
            .enumerate()
            .map(|(idx, repo)| (Self::normalize_repo_url(&repo.address), idx))
            .collect();

        for swifty_repo in &selected {
            let normalized = Self::normalize_repo_url(&swifty_repo.address);
            if let Some(existing_idx) = repo_index_by_address.get(&normalized).copied() {
                let space_binding = imported_space_id.as_deref().and_then(|space_id| {
                    self.swifty_space_binding_for_repo(space_id, &swifty_repo.address)
                        .map(|(entry_address, shared_path)| {
                            (space_id.to_string(), entry_address, shared_path)
                        })
                });
                let swifty_path = swifty_repo.mod_folder.trim();
                let repo = &mut self.repository_view_state.repositories[existing_idx];
                Self::add_swifty_repo_as_profile(repo, swifty_repo);
                if let Some((space_id, entry_address, shared_path)) = space_binding {
                    repo.repository_space_id = Some(space_id);
                    repo.repository_space_entry_address = Some(entry_address);
                    if !swifty_path.is_empty() {
                        repo.path = swifty_repo.mod_folder.clone();
                    } else if repo.path.trim().is_empty() && !shared_path.trim().is_empty() {
                        repo.path = shared_path;
                    }
                }
                imported_profiles += 1;

                // Refresh metadata for existing repos that haven't been fetched yet.
                if self.repository_view_state.repositories[existing_idx]
                    .servers
                    .is_empty()
                    && !existing_to_refresh.contains(&existing_idx)
                {
                    existing_to_refresh.push(existing_idx);
                }
                continue;
            }

            let path = if swifty_repo.mod_folder.trim().is_empty() {
                String::new()
            } else {
                swifty_repo.mod_folder.clone()
            };

            let mut repo = crate::ui::types::Repository {
                name: swifty_repo.name.clone(),
                address: swifty_repo.address.clone(),
                path,
                ..Default::default()
            };

            // Parse Swifty launch parameters into tick-box booleans + additional_params.
            if !swifty_repo.parameters.trim().is_empty() {
                scanner::apply_swifty_parameters(&mut repo, &swifty_repo.parameters);
            }

            // Carry over autocheck setting.
            if swifty_repo.autocheck {
                repo.auto_recheck_on_launch = Some(true);
            }

            // If we can derive an updater URL, store it on the repo.
            if let Some(urls) = derive_urls(&swifty_repo.address) {
                repo.app_update_url = urls.updater_url;
            }

            // Bind to the imported repository space if available.
            if let Some(ref space_id) = imported_space_id
                && let Some(space) = self.repository_spaces.iter().find(|s| s.id == *space_id)
            {
                let normalized_repo = Self::normalize_repo_url(&repo.address);
                let entry_match = space
                    .entries
                    .iter()
                    .find(|e| Self::normalize_repo_url(&e.address) == normalized_repo);

                if let Some(entry) = entry_match {
                    // Exact entry match - bind with entry address.
                    repo.repository_space_id = Some(space_id.clone());
                    repo.repository_space_entry_address = Some(entry.address.clone());
                } else if let Some(urls) = derive_urls(&repo.address) {
                    // No entry match - group under the space if the base URL matches.
                    let space_base = Self::normalize_repo_url(&space.source_base_url);
                    let repo_base = Self::normalize_repo_url(urls.base_url.trim_end_matches('/'));
                    if space_base == repo_base {
                        repo.repository_space_id = Some(space_id.clone());
                        repo.repository_space_entry_address = Some(repo.address.clone());
                    }
                }

                // Space members inherit the shared path unless Swifty carried
                // a per-repository folder, which becomes this repo's override.
                if repo.repository_space_id.is_some()
                    && repo.path.trim().is_empty()
                    && !space.shared_path.is_empty()
                {
                    repo.path = space.shared_path.clone();
                }
            }

            info!(
                "Importing Swifty repo '{}' (address={})",
                repo.name, repo.address
            );
            self.repository_view_state.repositories.push(repo);
            repo_index_by_address.insert(
                normalized,
                self.repository_view_state
                    .repositories
                    .len()
                    .saturating_sub(1),
            );
            imported += 1;
        }

        if imported > 0 || imported_profiles > 0 {
            self.save_repositories();
        }

        if imported > 0 {
            // Fetch remote repo.json for each imported repository to populate
            // servers, addons, and other metadata from the server.
            let total = self.repository_view_state.repositories.len();
            for i in (total - imported)..total {
                self.update_repository_from_url(i, ctx);
            }
        }

        // Refresh metadata for existing repos that were skipped as duplicates
        // but haven't had their remote metadata fetched yet.
        for idx in existing_to_refresh {
            self.update_repository_from_url(idx, ctx);
        }

        // --- 3. Set the app-level updater URL if not already configured. ---
        let updater_url = self
            .swifty_migration_state
            .detected_updater_url
            .trim()
            .to_string();
        if !updater_url.is_empty()
            && self.settings_view_state.app_update_url.trim().is_empty()
            && !self.settings_view_state.app_update_url_user_override
        {
            self.settings_view_state.app_update_url = updater_url.clone();
            info!("Migration: set app update URL to {}", updater_url);
        }

        // Also try the metadata-based auto-fill (picks up from spaces/repos).
        self.maybe_auto_fill_app_update_url_from_metadata();

        // --- 4. Migrate global Swifty settings (Arma path, temp path) if Foxy's are empty. ---
        let global = &self.swifty_migration_state.global_settings;
        if self.settings_view_state.arma3_directory.trim().is_empty()
            && !global.arma_path.trim().is_empty()
        {
            self.settings_view_state.arma3_directory = global.arma_path.clone();
            info!(
                "Migration: set Arma 3 directory from Swifty: {}",
                global.arma_path
            );
        }
        if self.settings_view_state.temp_directory.trim().is_empty()
            && !global.temp_path.trim().is_empty()
        {
            self.settings_view_state.temp_directory = global.temp_path.clone();
            info!(
                "Migration: set temp directory from Swifty: {}",
                global.temp_path
            );
        }

        self.settings_view_state.swifty_migration_offered = true;
        self.save_settings();

        self.swifty_migration_state.import_done = true;
        self.swifty_migration_state.imported_count = imported + imported_profiles;

        info!(
            "Swifty migration imported {} repositories and {} profiles",
            imported, imported_profiles
        );
        self.show_success_toast(self.t_fmt(
            "Successfully imported {count} repositories from Swifty.",
            &[("count", (imported + imported_profiles).to_string())],
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn swifty_repo(name: &str, parameters: &str) -> SwiftyDetectedRepo {
        SwiftyDetectedRepo {
            name: name.to_string(),
            address: "http://example.test/mods/main".to_string(),
            mod_folder: String::new(),
            parameters: parameters.to_string(),
            autocheck: false,
            selected: true,
        }
    }

    #[test]
    fn swifty_duplicate_profile_preserves_launch_parameters() {
        let profile = Foxy::profile_from_swifty_repo(
            &swifty_repo("Operation Alpha", "-skipIntro -mod=gm -window"),
            "Operation Alpha".to_string(),
        );

        assert_eq!(profile.name, "Operation Alpha");
        assert!(profile.skip_intro);
        assert!(profile.gm);
        assert_eq!(profile.additional_params, "-window");
    }

    #[test]
    fn swifty_duplicate_profile_uses_default_unique_name_when_empty() {
        let default_name = RepositoryProfile::default().name;
        let repo = Repository {
            profiles: vec![RepositoryProfile {
                name: default_name.clone(),
                ..Default::default()
            }],
            ..Default::default()
        };

        let name = Foxy::unique_swifty_profile_name(&repo, "  ");

        assert_eq!(name, format!("{} 1", default_name));
    }
}
