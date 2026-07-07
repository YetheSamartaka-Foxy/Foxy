use crate::ui::app::Foxy;
use crate::ui::types::HelpTab;
use eframe::egui::{
    self, Align, Button, CursorIcon, Frame, Label, Layout, Margin, RichText, ScrollArea, Ui, Vec2,
};
use log::info;

impl Foxy {
    pub fn render_help_view(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let help_margin = Margin {
            left: 8,
            right: 8,
            top: 8,
            bottom: 8,
        };

        Frame::NONE.inner_margin(help_margin).show(ui, |ui| {
            ui.vertical(|ui| {
                ui.horizontal(|ui| {
                    ui.heading(
                        RichText::new(self.t("Help"))
                            .size(self.settings_view_state.font_sizes.help_view.page_title as f32),
                    );

                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
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
                            ui.ctx()
                                .output_mut(|o| o.cursor_icon = CursorIcon::PointingHand);
                        }
                        if close_button.clicked() {
                            info!("Closing help view");
                            self.close_reference_view();
                        }
                    });
                });

                ui.separator();

                let available_size = ui.available_size_before_wrap();
                let pane_gap = 12.0;
                let total_width = available_size.x.max(0.0);
                let tab_width = (total_width * 0.28).clamp(240.0, 320.0);
                let content_width = (total_width - tab_width - pane_gap).max(0.0);
                let pane_height = available_size.y.max(320.0);

                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 0.0;

                    ui.allocate_ui_with_layout(
                        Vec2::new(tab_width, pane_height),
                        Layout::top_down(Align::Min),
                        |ui| {
                            Frame::NONE
                                .fill(self.color_card_bg())
                                .stroke(egui::Stroke::new(1.0, self.color_text_gray()))
                                .corner_radius(eframe::egui::CornerRadius::same(10))
                                .inner_margin(Margin::same(12))
                                .show(ui, |ui| {
                                    ui.set_width(tab_width - 24.0);
                                    ui.set_min_width(tab_width - 24.0);
                                    ui.set_min_height(pane_height - 24.0);
                                    ScrollArea::vertical()
                                        .id_salt("help_tab_list")
                                        .auto_shrink([false, false])
                                        .show(ui, |ui| {
                                            ui.with_layout(Layout::top_down(Align::Min), |ui| {
                                                self.render_help_tabs(ui);
                                            });
                                        });
                                });
                        },
                    );

                    ui.add_space(pane_gap);

                    ui.allocate_ui_with_layout(
                        Vec2::new(content_width, pane_height),
                        Layout::top_down(Align::Min),
                        |ui| {
                            Frame::NONE
                                .fill(self.color_card_bg())
                                .stroke(egui::Stroke::new(1.0, self.color_text_gray()))
                                .corner_radius(eframe::egui::CornerRadius::same(10))
                                .inner_margin(Margin::same(18))
                                .show(ui, |ui| {
                                    ui.set_width(content_width - 36.0);
                                    ui.set_min_width(content_width - 36.0);
                                    ui.set_min_height(pane_height - 36.0);
                                    ScrollArea::vertical()
                                        .id_salt(("help_content", self.current_help_tab.as_str()))
                                        .auto_shrink([false, false])
                                        .show(ui, |ui| {
                                            self.render_selected_help_tab(ui);
                                        });
                                });
                        },
                    );
                });
            });
        });
    }

    fn render_help_tabs(&mut self, ui: &mut Ui) {
        for tab in HelpTab::all_tabs() {
            let is_selected = self.current_help_tab == tab;
            let color = if is_selected {
                self.color_primary_accent()
            } else {
                self.color_main_bg()
            };

            let tab_button = ui.add_sized(
                Vec2::new(ui.available_width(), 40.0),
                Button::new(
                    RichText::new(self.t(tab.as_str()))
                        .color(self.color_text_normal())
                        .size(self.settings_view_state.font_sizes.help_view.tab_label as f32),
                )
                .fill(color),
            );

            if tab_button.hovered() {
                ui.ctx()
                    .output_mut(|o| o.cursor_icon = CursorIcon::PointingHand);
            }

            if tab_button.clicked() {
                self.current_help_tab = tab;
                info!("Switched help tab to {}", tab.as_str());
            }

            ui.add_space(4.0);
        }
    }

    fn render_selected_help_tab(&self, ui: &mut Ui) {
        match self.current_help_tab {
            HelpTab::Overview => self.render_help_page(
                ui,
                "Overview",
                &[
                    "Foxy keeps Arma 3 repositories, addon selection, launch settings, downloads, and recovery tools in one desktop app.",
                    "Foxy also includes a full command-line interface in the same binary for scripting and automation.",
                    "Foxy includes broad localization support with locale-aware pluralization, formatting, and dedicated font coverage for Arabic, Persian, Urdu, and Hebrew.",
                    "If you are new, read this page first and then continue to Getting started for the exact first steps.",
                ],
                &[
                    "Start in the repository list. That is the main screen where each repository card shows its current state, buttons, and server information.",
                    "Add repositories with + Add repository, then enter a repository URL or repository space URL and choose where the files should be stored.",
                    "Drag and drop repository cards to reorder them in the list.",
                    "Open a repository to review its addons, server entries, launch profiles, and maintenance actions.",
                    "Use Settings for global app behavior such as paths, language, backup rules, direct download, UI customization, and activity log visibility.",
                    "Foxy checks for app updates automatically or on demand. Use the Version Browser to upgrade, reinstall, or downgrade to any published release.",
                    "If you are switching from Swifty, the Swifty Migration wizard can import your existing repositories and settings automatically.",
                    "If a legacy setup contains multiple entries from the same source, Foxy can migrate them as separate profile variants instead of collapsing them into one setup.",
                    "The activity log in the bottom right shows what the app is doing now and keeps recent core messages available for troubleshooting.",
                    "The info icon opens About, the question mark opens this Help page, and the version number opens the changelog.",
                ],
            ),
            HelpTab::GettingStarted => self.render_help_page(
                ui,
                "Getting started",
                &[
                    "This is the simplest first-time setup path if you only want to get a repository installed and ready to launch.",
                    "If you are coming from Swifty, Foxy is fully backwards compatible with Swifty repositories and no server changes are required.",
                ],
                &[
                    "Set up paths first: point Arma 3 Directory to your game installation in Game space settings, and optionally configure Steam Directory, Temporary Directory, and Addon Backup Directory in Settings.",
                    "If your Arma 3 profile files are stored in Documents or OneDrive, configure a dedicated profile root so Foxy can launch with -profiles=<path> and avoid sync conflicts.",
                    "Click + Add repository on the main repository screen.",
                    "Paste either a repository URL or a repository space URL. If the source is a repository space, Foxy can help you pick one or more repositories from it.",
                    "Choose the local folder where the repository should live. This is where Foxy will check files, download updates, and launch from.",
                    "After the repository appears in the list, run Refresh to fetch remote metadata and build the first update plan.",
                    "Use Quick local check to compare your local content hashes and detect drift without rebuilding every stored hash.",
                    "When an update is ready, open the update view to review changed mods, file counts, and total size before downloading.",
                    "Use Launch to start Arma 3 with the selected profile. Use Join when a repository has a configured server and you want to connect after launch.",
                    "Use the Filter repositories field to search repositories by name when your list grows large.",
                    "Repository cards use a denser layout and can be reordered by drag and drop, so keep the repositories you use most near the top.",
                ],
            ),
            HelpTab::ChecksAndUpdates => self.render_help_page(
                ui,
                "Checks and updates",
                &[
                    "Foxy uses different checks for different jobs. The short version is: Refresh quickly compares the remote repository hash with the local one, Quick local check inspects your local files, and Recheck repository integrity performs a full remote fetch with complete hash recalculation.",
                ],
                &[
                    "Use Refresh when you want the latest remote metadata and update plan for a repository.",
                    "Use Quick local check to compare your local content hashes and detect drift without rebuilding every stored hash.",
                    "Use Recheck repository integrity when you need a full remote fetch and complete hash recalculation after major local changes or suspected corruption.",
                    "When an update is ready, open the update view to review changed mods, file counts, and total size before downloading.",
                    "Foxy prefers delta patches when available and automatically falls back to full file downloads if patch validation fails.",
                    "You can cancel an active sync while Foxy is hashing or downloading. Foxy aborts pending work, clears transient state, and returns progress to a clean idle state.",
                    "Active mod downloads bubble toward the top of the update list so the most advanced work remains visible during long syncs.",
                    "Download speed and ETA estimates use recent transfer history and are smoothed for slow disks or uneven networks.",
                    "Foxy cleans temporary download parts after aborted or failed transfers so stale partial files do not accumulate.",
                    "If something looks wrong after an interrupted or unusual update, start with Quick local check, then Refresh, and only use Recheck repository integrity if the normal checks do not resolve it.",
                ],
            ),
            HelpTab::ProfilesAndLaunch => self.render_help_page(
                ui,
                "Profiles and launch",
                &[
                    "Profiles are presets for how Arma 3 should start with a specific repository. They save time when you switch between different servers, unit setups, or optional addon combinations.",
                    "Profiles store launch parameters, DLC toggles, addon enablement, and optional extras so you can switch between presets quickly.",
                ],
                &[
                    "Open Repository Settings and select or create a profile for that repository.",
                    "Configure the profile by enabling or disabling addons, optional addons, external addons, DLC toggles, and extra launch parameters.",
                    "Use a custom profile root when you need Arma 3 to store profile data outside the default Documents path. Foxy passes that path to Arma 3 as -profiles=<path>.",
                    "Avoid OneDrive-synchronized profile paths. Foxy warns about OneDrive because cloud sync can lock or corrupt Arma 3 profile files.",
                    "Use Copy Profile when you want to start from an existing preset instead of building a new one from zero.",
                    "Use Export to share a profile as JSON via clipboard. Use Import to add a shared profile from clipboard to the current repository.",
                    "Profile names and editor mission names support UTF-8 text, so non-English names should display and launch correctly.",
                    "Back on the repository card, make sure the profile you want is selected, then click Launch to start Arma 3 with that configuration.",
                    "Use Launch to start Arma 3 with the selected profile. Use Join when a repository has a configured server and you want to connect after launch.",
                    "When joining a server, Foxy can warn if the server reports required addons that are available locally but disabled for the selected launch.",
                    "Offline servers do not show a Join action because there is no live target to connect to.",
                    "Server cards show each server's name, address, online status, and player count. Use Arrow Left and Arrow Right to navigate between them.",
                    "If Steam is not running, Foxy starts it automatically before launching Arma 3. Configure post-launch behavior in Settings to close or hide to tray.",
                ],
            ),
            HelpTab::RepositorySpacesAndAddons => self.render_help_page(
                ui,
                "Repository spaces and addons",
                &[
                    "Repository spaces are useful when a community publishes several related repositories under one shared manifest. They help with required repositories, optional repositories, and shared paths.",
                    "Repository spaces group multiple repositories under one manifest and can share a common local path for unit-wide setups.",
                ],
                &[
                    "Add a repository space the same way you add a normal repository: paste the space URL into the add dialog.",
                    "If the space contains multiple repositories, Foxy lets you choose which ones to add and where they should be stored.",
                    "Use a shared path when the repositories in that space should live under one common root folder.",
                    "Use the space toolbar to Recheck all, Quick local check all, or Update all repositories in the space at once.",
                    "If you added repositories individually before creating the space, use Scan existing repositories to find and move matches into the space.",
                    "Repository space names and labels allow longer values, so community names and grouping labels can be more descriptive.",
                    "External addons are addons that are available outside the repository itself. Foxy can discover them from repository-space shared paths, additional search folders from Game space settings, and Steam Workshop content when enabled.",
                    "When you edit addon selections for a repository, pay attention to whether an addon comes from the repository, from optional content, or from an external source.",
                    "Addons can be marked as favorites and filtered by favorite state, which is useful in large repositories with many optional or external addons.",
                    "Optional addon choices persist across refreshes and restarts, so disabled optional addons are not silently re-enabled or re-downloaded.",
                    "Repository metadata can mark client-side addons and server-required addons so launch prompts can explain what Foxy is about to enable.",
                ],
            ),
            HelpTab::EditorMissions => self.render_help_page(
                ui,
                "Editor missions",
                &[
                    "Editor Missions are shown inside the repository view so mission work stays close to the addons, profiles, and server setup it depends on.",
                ],
                &[
                    "Use the Editor Missions section to open, duplicate, delete, or launch Eden Editor for detected singleplayer and multiplayer mission folders.",
                    "Mission subfolders are scanned recursively, so nested mission organization is supported.",
                    "Use the terrain filter beside Show folders to narrow the mission list to one map when a profile contains many missions.",
                    "Use the mission context menu to remove addon dependencies from mission.sqm when you need a cleaner mission dependency list.",
                    "If additional or external addons are enabled, Foxy warns before launching Eden Editor because saving can write those addon dependencies into mission.sqm.",
                    "The editor launch warning lets you launch with addons, launch without additional/external addons, or cancel.",
                    "When an editor launch starts, Foxy shows a toast and suppresses repeated launch clicks while Arma 3 is starting.",
                    "Settings can hide the Editor Missions list globally, and repository settings can override that choice for a specific repository.",
                ],
            ),
            HelpTab::RecoveryAndTools => self.render_help_page(
                ui,
                "Recovery and tools",
                &[
                    "Foxy includes recovery tools for safer updates and a direct download tool for situations where you want files without full repository sync.",
                ],
                &[
                    "Enable addon backups before updates if you want recovery points. You can manage stored backups centrally and restore a specific addon from repository settings.",
                    "Use the Backup Manager in Settings to review stored backups, cleanup rules, and overall backup storage usage.",
                    "If you need to roll back one addon, open Repository Settings and restore the specific addon backup from there so Foxy knows the target path.",
                    "Use Force redownload repository in Repository Settings to remove local files and re-download everything when normal checks cannot resolve a problem.",
                    "Use Wipe repository database entries to clear cached metadata without deleting local files, then refresh to rebuild from scratch.",
                    "Wiping repository database entries also clears legacy metadata that could otherwise leave stale MD5 protocol or update-available banners behind.",
                    "Foxy asks for confirmation before profile deletion, profile reset, and full settings reset so destructive actions are harder to trigger by accident.",
                    "The Direct download page downloads repositories, addons, or individual files from a URL without syncing them into the database.",
                    "In Direct download, destination defaults to Temporary Directory and then to the Foxy config directory if Temporary Directory is empty.",
                    "Use Export logs to ZIP in Settings to package diagnostic logs into a timestamped Deflate-compressed archive for support.",
                    "If something looks wrong after an interrupted or unusual update, start with Quick local check, then Refresh, and only use Recheck repository integrity if the normal checks do not resolve it.",
                ],
            ),
            HelpTab::SettingsAndStatus => self.render_help_page(
                ui,
                "Settings and status",
                &[
                    "Settings control how Foxy behaves globally. The activity log and footer controls help you understand what the app is doing and where to look next.",
                ],
                &[
                    "Use the Application settings tab to configure language, Steam path, temporary storage, startup behavior, and other global options.",
                    "Set a global download speed limit in Application settings, or leave it unlimited.",
                    "Use Auto recheck repositories on launch and Auto quick scan for changes on launch in Application settings to start background verification automatically.",
                    "Repository settings can override these startup checks per repository when needed.",
                    "Use Game space settings to show or hide the Editor Missions list and Servers list for that game space, with per-repository overrides when needed.",
                    "Use the Customization tab if you want to change UI font sizes or palette colors without touching repository data.",
                    "Use the Additional search folders tab in Game space settings to register directories where Foxy discovers external addons.",
                    "Use the Cleanup tab to find and remove addons that are no longer used by any repository.",
                    "Use the TS3 Plugins tab in Game space settings to discover, install, and update TeamSpeak 3 plugins found in your repository addons.",
                    "Configure app updates in Application settings by choosing a Server or GitHub update source and enabling auto-check on launch.",
                    "If the app update URL field is empty, Foxy can fill it from repository-space metadata first and repository metadata second. A non-empty manual value is treated as your override.",
                    "Open the activity log from the bottom-right footer button when you want to see current work, recent core events, or troubleshooting details.",
                    "Log files older than 90 days are cleaned up automatically, and Foxy keeps up to 16 recent log files.",
                    "Use About for app information, Help for usage guidance, and the changelog for version-by-version feature and fix history.",
                    "If you are unsure what to do next, check the repository state banner first, then the activity log, then the relevant Help category for the screen you are using.",
                ],
            ),
            HelpTab::RendererAndPerformance => self.render_help_page(
                ui,
                "Renderer and performance",
                &[
                    "Foxy can use WGPU or Glow for the UI renderer. Auto is recommended because it allows Foxy to recover when a graphics driver or WGPU path is unstable.",
                ],
                &[
                    "Choose Auto, WGPU, or Glow in Application settings. Renderer changes take effect after restart.",
                    "If Foxy detects that the previous run crashed inside egui-wgpu, it writes a recovery marker and switches to Glow on the next launch.",
                    "When Foxy auto-switches to Glow, it shows a startup notice so you know why the renderer changed. Dismissing the notice clears the notice marker.",
                    "Glow provides an OpenGL fallback for systems where Vulkan, DirectX, drivers, overlays, or WGPU are unstable.",
                    "Foxy profiles runtime memory pressure as normal, constrained, or severe, then uses that tier when choosing hashing and download concurrency.",
                    "Hash scheduling applies safe caps for automatic and manual profiles when memory is constrained or severe.",
                    "Auto hash-profile benchmarking narrows candidate profiles under constrained resources and records cap reasons in logs.",
                    "Download scheduling adjusts large-file slots, small-file slots, active range requests, per-file workers, and chunk targets from runtime limits.",
                    "The hashing and sync pipelines include optimized file I/O paths, including faster local content hashing for gzip-like payloads.",
                    "The UI reduces unnecessary idle redraw work, and repository sync states avoid extra recalculation when nothing meaningful changed.",
                ],
            ),
            HelpTab::KeyboardShortcuts => self.render_help_page(
                ui,
                "Keyboard shortcuts",
                &[
                    "Foxy supports keyboard navigation throughout the interface. These shortcuts help you work faster without reaching for the mouse.",
                ],
                &[
                    "Press F1 to open Help from any screen.",
                    "Use Tab and Shift+Tab to move focus between interactive controls in any view.",
                    "Press Enter to activate the focused button, card, or default action in the current context.",
                    "Use Arrow Up and Arrow Down to navigate repository lists, addon lists, and other vertical item lists.",
                    "Use Arrow Left and Arrow Right to navigate between server cards in the repository detail view.",
                    "Press Escape to close modal dialogs, the help view, settings, and other overlay panels.",
                    "Use Ctrl+F or the filter box shortcut to quickly search and filter the repository list by name or address.",
                    "Repeated clicks on launch, update, and other long-running actions are suppressed while the operation is starting or already in progress.",
                    "In text fields, use standard editing shortcuts: Ctrl+A to select all, Ctrl+C to copy, Ctrl+V to paste, and Ctrl+Z to undo.",
                    "Most buttons and clickable surfaces show a pointer cursor on hover to indicate that they are interactive.",
                ],
            ),
            HelpTab::ThirdPartyOverlays => self.render_help_page(
                ui,
                "Third-party overlays",
                &[
                    "Third-party overlays such as NVIDIA GeForce Experience, NVIDIA App, Discord, Steam, Bandicam, and similar screen-capture or recording tools can attach to Foxy on startup and cause unwanted pop-ups, performance issues, or rendering glitches.",
                    "Foxy is a desktop application, not a game, so it does not benefit from in-game overlays. You can safely exclude Foxy from these overlays without losing any functionality.",
                ],
                &[
                    "NVIDIA GeForce Experience or NVIDIA App: open the app, go to Settings, find the in-game overlay or Games and Apps section, and add Foxy to the excluded applications list.",
                    "If you do not use NVIDIA features like instant replay, screenshots, or recording, you can disable the NVIDIA in-game overlay globally in NVIDIA App settings.",
                    "NVIDIA classifies any DirectX 12 or Vulkan application as a potential game. This behavior is not specific to Foxy and there is no supported way for Foxy to opt out programmatically.",
                    "Discord overlay: open Discord, go to User Settings, Game Activity, then either remove Foxy from the detected games list or disable the overlay for it.",
                    "Steam overlay: if Foxy was launched through Steam, open Steam, go to Settings, In-Game, and disable the Steam overlay for Foxy via its properties. Foxy is normally not launched through Steam, so this usually does not apply.",
                    "Screen-capture tools such as Bandicam, OBS, ShadowPlay, and Xbox Game Bar: exclude Foxy from their auto-capture lists. On Windows you can also disable Xbox Game Bar capture for Foxy under Windows Settings, Gaming, Captures.",
                    "If startup logs mention Vulkan layers such as VK_LAYER_bandicam_helper, update or disable Bandicam and other screen-capture overlays, then restart Foxy.",
                    "After changing overlay settings, fully close and relaunch Foxy so the overlay hooks do not re-attach to the running process.",
                ],
            ),
            HelpTab::Troubleshooting => self.render_help_page(
                ui,
                "Troubleshooting",
                &[
                    "If something is not working as expected, these steps cover the most common issues and how to resolve them.",
                ],
                &[
                    "If a repository shows an unexpected state, run Quick local check first, then Refresh. This resolves most drift and stale-metadata issues.",
                    "If downloads fail repeatedly, check your internet connection, verify the repository URL is reachable in a browser, and review the activity log for specific error messages.",
                    "If the app reports hash mismatches after an update, use Recheck repository integrity to fetch fresh remote data and rebuild stored checksums from scratch.",
                    "If a repository is stuck in an updating state, check the activity log for errors. You can use Wipe database for that repository and re-add it as a last resort.",
                    "If external addons are not detected, verify that the search folders are configured in Game space settings and that Steam Workshop scanning is enabled if you use Workshop content.",
                    "If launch fails or the wrong addons load, verify the selected profile in Repository Settings and check that addon paths and DLC toggles are correct.",
                    "If Foxy rejects a profile path, check whether it is inside OneDrive or contains characters that are unsafe for cross-platform file handling.",
                    "If the NVIDIA GeForce Experience or NVIDIA App overlay pops up every time Foxy launches, add Foxy to the excluded applications list in NVIDIA App settings. See the Third-party overlays help tab for details.",
                    "For other overlays such as Discord, Steam, Bandicam, OBS, or Xbox Game Bar, see the Third-party overlays help tab for per-tool exclusion steps.",
                    "If startup logs mention Vulkan layers such as VK_LAYER_bandicam_helper, update or disable Bandicam and other screen-capture overlays, then restart Foxy.",
                    "If Foxy starts with Glow after a crash, review the Renderer and performance help tab and keep Auto selected unless you have a specific reason to force WGPU.",
                    "Use Force redownload repository in Repository Settings as a last resort when normal checks and integrity rechecks do not resolve persistent issues.",
                    "Check the activity log in the bottom-right corner for detailed core messages. You can copy the full log to clipboard for sharing with support.",
                    "Log files are stored in the Foxy config directory under the logs folder. Use Open log folder in Settings for quick access.",
                ],
            ),
        }
    }

    fn render_help_page(
        &self,
        ui: &mut Ui,
        title_key: &str,
        intro_keys: &[&str],
        step_keys: &[&str],
    ) {
        let fonts = &self.settings_view_state.font_sizes.help_view;

        ui.heading(RichText::new(self.t(title_key)).size(fonts.section_title as f32));
        ui.add_space(4.0);

        for paragraph_key in intro_keys {
            ui.add(Label::new(RichText::new(self.t(paragraph_key)).size(fonts.body as f32)).wrap());
            ui.add_space(8.0);
        }

        ui.label(
            RichText::new(self.t("Step by step"))
                .strong()
                .size(fonts.body as f32),
        );
        ui.add_space(6.0);

        for (index, step_key) in step_keys.iter().enumerate() {
            ui.horizontal_wrapped(|ui| {
                ui.label(
                    RichText::new(format!("{}. ", index + 1))
                        .strong()
                        .size(fonts.body as f32),
                );
                ui.add_space(2.0);
                ui.add(Label::new(RichText::new(self.t(step_key)).size(fonts.body as f32)).wrap());
            });
            ui.add_space(8.0);
        }
    }
}
