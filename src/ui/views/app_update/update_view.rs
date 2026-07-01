use super::*;

impl Foxy {
    pub(super) fn render_download_install_status(&mut self, ui: &mut Ui) {
        match &self.app_update_status {
            UpdateCheckStatus::Downloading {
                progress,
                bytes_done,
                bytes_total,
            } => {
                ui.label(self.t("Downloading installer..."));
                ui.add(ProgressBar::new(*progress).show_percentage());
                ui.label(format!(
                    "{} / {} MB",
                    format_mb(*bytes_done),
                    format_mb(*bytes_total),
                ));
            }
            UpdateCheckStatus::Verifying => {
                ui.label(self.t("Verifying installer integrity..."));
                ui.spinner();
            }
            UpdateCheckStatus::ReadyToInstall { installer_path } => {
                let path = installer_path.clone();
                ui.label(
                    RichText::new(self.t("Installer downloaded and verified."))
                        .color(self.color_success()),
                );
                ui.add_space(8.0);
                ui.label(self.t(
                    "Foxy will close and the installer will run. After installation, Foxy will restart automatically.",
                ));
                ui.add_space(8.0);
                let install_btn = ui.add_sized(
                    Vec2::new(200.0, 36.0),
                    Button::new(
                        RichText::new(self.t("Install and Restart"))
                            .color(self.color_text_normal()),
                    ),
                );
                if install_btn.hovered() {
                    ui.ctx()
                        .output_mut(|o| o.cursor_icon = CursorIcon::PointingHand);
                }
                if install_btn.clicked()
                    && let Err(e) = app_update::launch_installer(&path, true)
                {
                    log::error!("Failed to launch installer: {}", e);
                    self.app_update_status =
                        UpdateCheckStatus::Failed(format!("Failed to launch installer: {}", e));
                }
            }
            UpdateCheckStatus::Failed(msg) => {
                ui.label(
                    RichText::new(format!("{}: {}", self.t("Error"), msg))
                        .color(self.color_text_error()),
                );
                ui.add_space(8.0);
                let retry_btn = ui.add(Button::new(self.t("Retry")));
                if retry_btn.clicked() {
                    self.start_update_check();
                }
            }
            _ => {}
        }
    }

    // -----------------------------------------------------------------------
    // Shared: view header (heading + close button + separator)
    // -----------------------------------------------------------------------

    pub(super) fn render_reference_view_header(&mut self, ui: &mut Ui, title: &str) {
        ui.horizontal(|ui| {
            ui.heading(title);

            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let close_btn = ui.add_sized(
                    Vec2::new(30.0, 30.0),
                    Button::new(RichText::new("X").color(self.color_text_normal()))
                        .fill(self.color_main_bg()),
                );
                if close_btn.hovered() {
                    ui.ctx()
                        .output_mut(|o| o.cursor_icon = CursorIcon::PointingHand);
                }
                if close_btn.clicked() {
                    self.close_reference_view();
                }
            });
        });
        ui.separator();
    }

    // -----------------------------------------------------------------------
    // App Update View (notification modal for upgrades)
    // -----------------------------------------------------------------------

    pub fn render_app_update_view(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let margin = Margin {
            left: 15,
            right: 15,
            top: 10,
            bottom: 10,
        };

        Frame::NONE.inner_margin(margin).show(ui, |ui| {
            ui.vertical(|ui| {
                self.render_reference_view_header(ui, &self.t("App Update"));

                match &self.app_update_status {
                    UpdateCheckStatus::Idle | UpdateCheckStatus::Checking => {
                        ui.label(self.t("Checking for updates..."));
                        ui.spinner();
                    }
                    UpdateCheckStatus::UpToDate(_) => {
                        ui.label(
                            RichText::new(self.t("You are running the latest version."))
                                .color(self.color_success()),
                        );
                        ui.label(format!(
                            "{}: {}",
                            self.t("Current version"),
                            env!("CARGO_PKG_VERSION")
                        ));
                    }
                    UpdateCheckStatus::Available(info) => {
                        let info = info.clone();
                        self.render_update_available(ui, &info);
                    }
                    _ => {
                        self.render_download_install_status(ui);
                    }
                }
            });
        });
    }

    pub(super) fn render_update_available(
        &mut self,
        ui: &mut Ui,
        info: &app_update::AppUpdateInfo,
    ) {
        let current = env!("CARGO_PKG_VERSION");
        let latest = &info.manifest.latest;

        ui.horizontal(|ui| {
            ui.label(format!("{}: v{}", self.t("Current version"), current));
            ui.label("  ->  ");
            ui.label(RichText::new(format!("v{}", latest)).color(self.color_primary_accent()));
        });

        ui.add_space(8.0);

        ui.label(RichText::new(self.t("Changelog")).strong());

        ScrollArea::vertical()
            .max_height(ui.available_height() - 60.0)
            .auto_shrink([false; 2])
            .show(ui, |ui| {
                if self.app_update_changelogs.is_empty() && !self.app_update_changelogs_requested {
                    self.request_changelogs(&info.source_base_url, &info.manifest.versions);
                    ui.label(self.t("Loading changelog..."));
                    ui.spinner();
                } else if self.app_update_changelogs.is_empty()
                    && !self.app_update_changelog_loading.is_empty()
                {
                    ui.label(self.t("Loading changelog..."));
                    ui.spinner();
                } else if self.app_update_changelogs.is_empty() {
                    ui.label(self.t("Changelog not available."));
                } else {
                    self.render_changelogs_list(ui);
                }
            });

        ui.add_space(8.0);

        let platform_key = app_update::current_platform_key();
        let latest_entry = info.manifest.versions.iter().find(|v| v.version == *latest);

        if let Some(entry) = latest_entry {
            if let Some(platform) = entry.platforms.get(platform_key) {
                let btn_text = format!(
                    "{} ({} MB)",
                    self.t("Download Update"),
                    format_mb(platform.installer_size)
                );
                let download_btn = ui.add_sized(
                    Vec2::new(220.0, 36.0),
                    Button::new(RichText::new(btn_text).color(self.color_text_normal())),
                );
                if download_btn.hovered() {
                    ui.ctx()
                        .output_mut(|o| o.cursor_icon = CursorIcon::PointingHand);
                }
                if download_btn.clicked() {
                    self.start_installer_download(entry);
                }
            } else {
                ui.label(
                    RichText::new(format!(
                        "{} {}",
                        self.t("No installer available for"),
                        platform_key
                    ))
                    .color(self.color_text_error()),
                );
            }
        }
    }

    // -----------------------------------------------------------------------
    // Version Browser View (all versions, with downgrade support)
    // -----------------------------------------------------------------------
}
