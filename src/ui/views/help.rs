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
                    "Foxy keeps game mod repositories, addon selection, launch settings, downloads, and recovery tools in one desktop app. Arma 3, Total War: WARHAMMER III, and Arma Reforger are supported, and Generic game covers other Steam games.",
                    "Each game lives in its own game space with separate repositories, settings, and database. See the Game spaces help tab for details.",
                    "Foxy also includes a full command-line interface in the same binary for scripting and automation.",
                    "Foxy includes broad localization support with locale-aware pluralization, formatting, and dedicated font coverage for Arabic, Persian, Urdu, and Hebrew.",
                    "If you are new, read this page first and then continue to Getting started for the exact first steps.",
                ],
                &[
                    "Start in the repository list. That is the main screen where each repository card shows its current state, buttons, and server information.",
                    "The panel at the top of the sidebar shows the open game space. Use Switch to change game spaces, the gear icon to open Game space settings, and the game logo to open the game space overview.",
                    "Add repositories with + Add repository, then enter a repository URL or repository space URL and choose where the files should be stored.",
                    "Drag and drop repository cards to reorder them in the list.",
                    "Open a repository to review its addons, server entries, launch profiles, and maintenance actions.",
                    "Use Settings for global app behavior such as paths, language, backup rules, direct download, UI customization, and activity log visibility.",
                    "Use Game space settings for everything that belongs to one game, such as the game directory, launch checks, profiles, Steam Workshop mods, and additional search folders.",
                    "Use the Scheduling tab in Settings to run rechecks and downloads at a chosen time, and the Benchmarks tab to save and compare how your checks and updates performed.",
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
                    "Check that the right game is open. The top of the sidebar shows the active game space; use Switch there to create or open a game space for another game.",
                    "Set up paths first: point Arma 3 Directory to your game installation in Game space settings, and optionally configure Steam Directory, Temporary Directory, and Addon Backup Directory in Settings.",
                    "For other games, open Game space settings from the gear icon in the sidebar and set the game directory, or use Auto-detect when it is available to find it from your Steam library.",
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
            HelpTab::GameSpaces => self.render_help_page(
                ui,
                "Game spaces",
                &[
                    "A game space is a separate workspace for one game, with its own repositories, repository spaces, game settings, mod stores, benchmarks, and database. Exactly one game space is open at a time, and Foxy reopens the last one on the next launch.",
                    "Supported games are Arma 3, Total War: WARHAMMER III, and Arma Reforger. Generic game covers other Steam games: you set the executable, launch arguments, mods manifest file, and optional Steam App ID yourself.",
                ],
                &[
                    "The panel at the top of the sidebar shows the open game space. Click Switch to open the Game spaces list, the gear icon to open Game space settings, or the game logo to open the game space overview.",
                    "In the Game spaces list, use Create game space, choose the game, and enter a name. You can create several game spaces for the same game to keep separate setups apart.",
                    "Use Open game space to switch right away without restarting Foxy. Pending changes are saved first. Foxy does not switch while downloads or scans are running, so let them finish first.",
                    "Remove game space deletes only the Foxy workspace: its repository list, game settings, and local database. Game installs and downloaded mods stay on disk.",
                    "Game space settings show a tab for the game itself plus Profiles, Steam Workshop, Additional search folders, and TS3 Plugin. Tabs only appear when the game supports them.",
                    "You can open the settings of a game space that is not open. Changes are saved to that game space and take effect when you open it. Profiles, Steam Workshop, and TS3 Plugin need the game space to be open.",
                    "Rename in Game space settings changes only the name shown in Foxy. The game space folder and its data stay where they are.",
                    "The game space overview shows repositories, repository spaces, unique addons, mods on disk, last launch and update times, TeamSpeak 3 status, and your recent benchmarks.",
                    "Language, renderer, backups, and other Application settings are shared by all game spaces. Scheduled jobs, cleanup folders, additional search folders, and pending updates belong to the game space they were created in.",
                    "When you upgrade from a Foxy version without game spaces, your existing setup moves into an Arma 3 game space automatically. The previous files are kept as .pre-gamespaces.bak copies.",
                    "Each game space stores its data under games/<game space> in the Foxy config directory. Only one Foxy window or command can use a game space at a time.",
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
                    "With Check Steam is running before launching turned on in Game space settings, Foxy warns when Steam is not running and offers to start it. Check TeamSpeak is running before joining does the same for TeamSpeak 3. Configure post-launch behavior in Settings to close or hide to tray.",
                    "Use the Profiles tab in Game space settings to clone, rename, or delete Arma 3 player profiles. Deleted profiles are moved to the Foxy backup directory, and changes are blocked while Arma 3 is running.",
                    "For Total War: WARHAMMER III, Foxy writes the enabled mods to used_mods.txt in the game directory before launch. For Arma Reforger, Foxy builds the -addons and -addonsDir launch parameters from the enabled addons.",
                    "Enabled extra files are copied into the game directory before each launch. See the Steam Workshop and extra files help tab.",
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
            HelpTab::SteamWorkshopAndExtraFiles => self.render_help_page(
                ui,
                "Steam Workshop and extra files",
                &[
                    "For Steam games, Foxy manages Steam Workshop mods next to repository content. Open Game space settings and choose the Steam Workshop tab while that game space is open.",
                    "Managed extra files and .foxypack config packs cover files that are not mods and let you move a whole setup to another machine.",
                ],
                &[
                    "The game space needs a Steam App ID. Built-in games have one already; for Generic game, set it in Game space settings.",
                    "Use Import share code to paste Workshop ids, Workshop links, or a pipe-separated list from a friend. Keep Subscribe and download through Steam on to download the mods, and turn on Freeze each mod after downloading to pin them right away.",
                    "Copy share code copies your enabled mods as a pipe-separated list that other mod managers understand.",
                    "Paste a friend's share code into Compare with to check it against your setup. Foxy shows when your enabled mods match, when the load order differs, and offers Import missing to download only the mods you do not have yet.",
                    "The state checksum covers your enabled mods, the game build, and load order. Click it to copy; a friend with the same checksum is running the same setup.",
                    "Enable or disable each mod in the list, and use Load earlier and Load later to change the load order.",
                    "Freeze keeps a private copy of a mod at its current version so a Workshop update cannot change your setup. Unfreeze follows Steam updates again, and Freeze all freezes every mod at once.",
                    "Export bundle writes a .foxyshare file with the mod list and every frozen copy. Import bundle reads a .foxyshare file a friend sent you.",
                    "Use Refresh to fetch titles, sizes, and update times from Steam, and Open to view a mod's Workshop page.",
                    "Remove takes a mod out of this game space. Turn on Also delete the downloaded files and frozen copies to free the disk space as well.",
                    "Managed extra files store config folders and other files that are not mods inside Foxy, and copy the enabled ones into the game directory before each launch. Manage them on the command line with foxy config extra-file.",
                    "A .foxypack config pack exports a game space's repositories, Workshop ids, profiles, and extra files so you can import them on another machine. Packs reference mods by id and never carry mod files. Use foxy config export and foxy config import.",
                    "Arma Reforger Workshop addons are managed by GUID on the command line with foxy game reforger, including addon freezing.",
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
                    "Use the TS3 Plugin tab in Game space settings to discover, install, and update TeamSpeak 3 plugins found in your repository addons.",
                    "Use the Scheduling tab to recheck and download updates at a chosen time, once or on a recurring schedule, for a single repository, a repository space, or a custom selection. Jobs only run while Foxy is open.",
                    "A scheduled job can close Foxy or shut down the PC when it finishes, optionally only if everything succeeded. Shutdown starts a 60-second countdown that you can cancel.",
                    "Turn on Extended diagnostics logging in Application settings when you report a performance problem. It writes detailed hashing, download, database, disk, and memory diagnostics to the log files.",
                    "Show FPS counter and Show memory diagnostics icon in footer in Application settings help diagnose UI performance and memory use.",
                    "Configure app updates in Application settings by choosing a Server or GitHub update source and enabling auto-check on launch.",
                    "If the app update URL field is empty, Foxy can fill it from repository-space metadata first and repository metadata second. A non-empty manual value is treated as your override.",
                    "Open the activity log from the bottom-right footer button when you want to see current work, recent core events, or troubleshooting details.",
                    "Log files older than 90 days are cleaned up automatically, and Foxy keeps up to 16 recent log files.",
                    "Use About for app information, Help for usage guidance, and the changelog for version-by-version feature and fix history.",
                    "If you are unsure what to do next, check the repository state banner first, then the activity log, then the relevant Help category for the screen you are using.",
                ],
            ),
            HelpTab::Benchmarks => self.render_help_page(
                ui,
                "Benchmarks",
                &[
                    "Benchmarks record how a recheck, update, or redownload performed on your computer, so you can compare runs over time or attach them when you report a performance problem.",
                    "Benchmarks are stored per game space, together with their metrics, charts, and the matching slice of the log.",
                ],
                &[
                    "Open Settings, choose the Benchmarks tab, and turn on Enable benchmarks. Extended diagnostics logging stays on while benchmarks are enabled, so the log files grow faster.",
                    "Start a recheck, update, or redownload yourself. When it finishes, Foxy asks whether to save the run. Choose Save benchmark to keep it or Discard to skip it.",
                    "The Benchmarks tab lists your saved runs. Use Search benchmarks, the action, outcome, and repository filters, and the sort order to find the run you need.",
                    "Open a benchmark to see stage durations, files checked, download sizes and speeds, patch savings, peak memory, average CPU, power source, and charts for CPU, memory, disk writes, and transfer rates.",
                    "Speed of light shows how close each operation ran to its reference, such as a bound, the run's own peak, or a nominal device rate. Cancelled or failed runs are excluded from best-case comparison.",
                    "Use Add notes about this run and Save notes to remember what was different, for example another disk, network, or hashing profile.",
                    "Mark runs as Favourite, hide runs you do not need with Hide benchmark, or delete them with Remove benchmark. Show hidden brings hidden runs back into the list.",
                    "Select two benchmarks and choose Compare selected to see them side by side. Swap A and B exchanges the sides, and Oldest as A keeps the older run on the A side.",
                    "Use Export benchmark to ZIP to package a benchmark with its charts and log slice for support, and Open benchmarks folder to see the stored files.",
                    "The game space overview lists your recent benchmarks. Use Open benchmarks there to jump to the Benchmarks tab.",
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
                    "Choose a Hashing profile in Application settings: Conservative for constrained systems, Balanced for mixed systems, Aggressive for systems that can sustain high concurrency, or Auto to benchmark initial hashing work and pick the fastest profile.",
                    "Skip unchanged files after a database reset lets Foxy trust files it already verified and that Windows shows as untouched since, instead of reading them again. An integrity recheck always reads every file.",
                    "Foxy profiles runtime memory pressure as normal, constrained, or severe, then uses that tier when choosing hashing and download concurrency.",
                    "Hash scheduling applies safe caps for automatic and manual profiles when memory is constrained or severe.",
                    "Auto hash-profile benchmarking narrows candidate profiles under constrained resources and records cap reasons in logs.",
                    "Download scheduling adjusts large-file slots, small-file slots, active range requests, per-file workers, and chunk targets from runtime limits.",
                    "The hashing and sync pipelines include optimized file I/O paths, including faster local content hashing for gzip-like payloads.",
                    "The UI reduces unnecessary idle redraw work, and repository sync states avoid extra recalculation when nothing meaningful changed.",
                    "To measure how a check or update performs on your computer, save it as a benchmark. See the Benchmarks help tab.",
                ],
            ),
            HelpTab::CommandLine => self.render_help_page(
                ui,
                "Command line",
                &[
                    "The same Foxy program also works as a command-line tool, so you can script checks, updates, launches, and configuration. It uses the same config directory and game spaces as the desktop app.",
                ],
                &[
                    "Run foxy --help to list every command, and add --help after any command to see its options.",
                    "Use foxy repo, foxy sync, foxy addon, foxy profile, and foxy space to manage repositories, addons, launch profiles, and repository spaces.",
                    "Use foxy game list, use, create, remove, and launch to manage game spaces. Commands work on the active game space.",
                    "Use foxy workshop to add, import, enable, freeze, share, and remove Steam Workshop items, and foxy game reforger for Arma Reforger Workshop addons.",
                    "Use foxy config export and foxy config import for .foxypack config packs, and foxy config extra-file to manage extra files.",
                    "Use foxy launch to start the game with a repository and profile, and foxy direct-download to download content by URL without database sync.",
                    "Add --json for machine-readable output, --dry-run to preview a change without writing it, and --yes to confirm destructive operations.",
                    "Use --quiet to reduce output, or --no-progress to turn off live progress updates, which works better with screen readers.",
                    "Use --config-dir or the FOXY_CONFIG_DIR environment variable to point Foxy at a different config directory.",
                    "Only one Foxy window or command can use a game space at a time. If the game space is already in use, the command stops with a database busy error.",
                    "Use foxy ui to start the desktop app from a terminal.",
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
                    "Press F2 to open or close Settings, and F3 to show or hide the activity log.",
                    "Press F4 to open memory diagnostics when Show memory diagnostics icon in footer is turned on.",
                    "Use Tab and Shift+Tab to move focus between interactive controls in any view.",
                    "Press Enter to activate the focused button, card, or default action in the current context.",
                    "Use Arrow Up and Arrow Down to navigate repository lists, addon lists, and other vertical item lists.",
                    "Use Arrow Left and Arrow Right to navigate between server cards in the repository detail view.",
                    "Press Escape to close modal dialogs, the help view, settings, and other overlay panels.",
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
                    "If a repository is stuck in an updating state, check the activity log for errors. As a last resort, use Wipe repository database entries in Repository Settings and refresh.",
                    "If your repositories seem to be missing, check which game space is open at the top of the sidebar. Each game space has its own repository list.",
                    "If Foxy reports that another Foxy process is already using this game space's database, close the other Foxy window or command first. Only one can use a game space at a time.",
                    "If Foxy asks to wipe and rebuild the database after an update, the stored database was made by an incompatible version. Rebuilding keeps your downloaded mods on disk, and the next check verifies them again.",
                    "If the Steam Workshop tab is missing or empty, open that game space first and make sure it has a Steam App ID in Game space settings.",
                    "If external addons are not detected, verify that the search folders are configured in Game space settings and that Steam Workshop scanning is enabled if you use Workshop content.",
                    "If launch fails or the wrong addons load, verify the selected profile in Repository Settings and check that addon paths and DLC toggles are correct.",
                    "If Foxy rejects a profile path, check whether it is inside OneDrive or contains characters that are unsafe for cross-platform file handling.",
                    "If the NVIDIA GeForce Experience or NVIDIA App overlay pops up every time Foxy launches, add Foxy to the excluded applications list in NVIDIA App settings. See the Third-party overlays help tab for details.",
                    "For other overlays such as Discord, Steam, Bandicam, OBS, or Xbox Game Bar, see the Third-party overlays help tab for per-tool exclusion steps.",
                    "If startup logs mention Vulkan layers such as VK_LAYER_bandicam_helper, update or disable Bandicam and other screen-capture overlays, then restart Foxy.",
                    "If Foxy starts with Glow after a crash, review the Renderer and performance help tab and keep Auto selected unless you have a specific reason to force WGPU.",
                    "Use Force redownload repository in Repository Settings as a last resort when normal checks and integrity rechecks do not resolve persistent issues.",
                    "Check the activity log in the bottom-right corner for detailed core messages. You can copy the full log to clipboard for sharing with support.",
                    "When a check or update is slow, turn on Enable benchmarks in the Benchmarks settings tab, repeat the action, save the benchmark, and share it with Export benchmark to ZIP.",
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
