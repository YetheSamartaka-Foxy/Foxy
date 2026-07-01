use super::*;

impl Foxy {
    pub fn render_version_browser_view(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let margin = Margin {
            left: 15,
            right: 15,
            top: 10,
            bottom: 10,
        };

        Frame::NONE.inner_margin(margin).show(ui, |ui| {
            ui.vertical(|ui| {
                self.render_reference_view_header(ui, &self.t("Version Browser"));

                match &self.app_update_status {
                    UpdateCheckStatus::Idle => {
                        ui.label(self.t("No update source configured or not yet checked."));
                        if self.app_update_source_configured() {
                            let check_btn = ui.add(Button::new(self.t("Check Now")));
                            if check_btn.clicked() {
                                self.start_update_check();
                            }
                        }
                    }
                    UpdateCheckStatus::Checking => {
                        ui.label(self.t("Checking for updates..."));
                        ui.spinner();
                    }
                    UpdateCheckStatus::Available(_) | UpdateCheckStatus::UpToDate(_) => {
                        let info_opt = match &self.app_update_status {
                            UpdateCheckStatus::Available(info)
                            | UpdateCheckStatus::UpToDate(info) => Some(info.clone()),
                            _ => None,
                        };
                        self.render_version_list(ui, info_opt.as_ref());
                    }
                    _ => {
                        self.render_download_install_status(ui);
                    }
                }
            });
        });
    }

    pub(super) fn render_version_list(
        &mut self,
        ui: &mut Ui,
        info: Option<&app_update::AppUpdateInfo>,
    ) {
        let current_version = env!("CARGO_PKG_VERSION");
        let platform_key = app_update::current_platform_key();

        let empty_versions = Vec::new();
        let versions = info
            .map(|i| i.manifest.versions.as_slice())
            .unwrap_or(&empty_versions);

        let latest = info.map(|i| i.manifest.latest.as_str()).unwrap_or("");

        if versions.is_empty() {
            ui.label(self.t("No versions available."));
            return;
        }

        ScrollArea::vertical()
            .auto_shrink([false; 2])
            .show(ui, |ui| {
                for entry in versions {
                    let is_current = entry.version == current_version;
                    let is_latest = entry.version == latest;
                    let is_older = semver::Version::parse(&entry.version)
                        .ok()
                        .zip(semver::Version::parse(current_version).ok())
                        .is_some_and(|(v, c)| v < c);

                    let card_frame = Frame::NONE
                        .inner_margin(Margin::same(10))
                        .fill(self.color_card_bg())
                        .corner_radius(4.0);

                    card_frame.show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new(format!("v{}", entry.version))
                                    .strong()
                                    .size(16.0),
                            );

                            if is_current {
                                ui.label(
                                    RichText::new(format!(" [{}]", self.t("Current")))
                                        .color(self.color_success()),
                                );
                            } else if is_latest {
                                ui.label(
                                    RichText::new(format!(" [{}]", self.t("Latest")))
                                        .color(self.color_primary_accent()),
                                );
                            } else if is_older {
                                ui.label(
                                    RichText::new(format!(" [{}]", self.t("Older")))
                                        .color(self.color_text_gray()),
                                );
                            } else {
                                ui.label(
                                    RichText::new(format!(" [{}]", self.t("Newer")))
                                        .color(self.color_primary_accent()),
                                );
                            }

                            if let Some(platform) = entry.platforms.get(platform_key) {
                                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                    ui.label(
                                        RichText::new(format!(
                                            "{} MB",
                                            format_mb(platform.installer_size)
                                        ))
                                        .color(self.color_text_dim()),
                                    );
                                });
                            }
                        });

                        // Collapsible changelog
                        let ver_str = &entry.version;
                        let has_changelog = self
                            .app_update_changelogs
                            .iter()
                            .any(|c| c.version == *ver_str);

                        let header_id = ui.make_persistent_id(format!("cl_{}", ver_str));
                        egui::collapsing_header::CollapsingState::load_with_default_open(
                            ui.ctx(),
                            header_id,
                            false,
                        )
                        .show_header(ui, |ui| {
                            ui.label(self.t("Changelog"));
                        })
                        .body(|ui| {
                            if has_changelog {
                                let cl = self
                                    .app_update_changelogs
                                    .iter()
                                    .find(|c| c.version == *ver_str)
                                    .cloned();
                                if let Some(cl) = cl {
                                    render_single_changelog(ui, &cl, self);
                                }
                            } else if let Some(info) = info {
                                self.fetch_single_changelog(
                                    &info.source_base_url,
                                    &entry.changelog,
                                    ver_str,
                                );
                                ui.label(self.t("Loading..."));
                                ui.spinner();
                            }
                        });

                        // Install/reinstall button
                        if entry.platforms.contains_key(platform_key) {
                            ui.add_space(4.0);

                            if is_older {
                                ui.label(
                                    RichText::new(format!(
                                        "{} (v{} -> v{})",
                                        self.t("This will install an older version"),
                                        current_version,
                                        entry.version
                                    ))
                                    .color(self.color_warn()),
                                );
                            }

                            let btn_label = if is_current {
                                self.t("Reinstall")
                            } else {
                                self.t("Download & Install")
                            };

                            let install_btn = ui.add(Button::new(btn_label));
                            if install_btn.hovered() {
                                ui.ctx()
                                    .output_mut(|o| o.cursor_icon = CursorIcon::PointingHand);
                            }
                            if install_btn.clicked() {
                                self.start_installer_download(entry);
                            }
                        } else if !is_current {
                            ui.label(
                                RichText::new(self.t("Not available for your platform"))
                                    .color(self.color_text_dim()),
                            );
                        }
                    });

                    ui.add_space(4.0);
                }
            });
    }

    // -----------------------------------------------------------------------
    // Shared changelog rendering
    // -----------------------------------------------------------------------

    pub(super) fn render_changelogs_list(&self, ui: &mut Ui) {
        let mut sorted: Vec<&ChangelogVersion> = self.app_update_changelogs.iter().collect();
        sorted.sort_by(|a, b| {
            let va = semver::Version::parse(&a.version).ok();
            let vb = semver::Version::parse(&b.version).ok();
            match (vb, va) {
                (Some(b), Some(a)) => b.cmp(&a),
                _ => std::cmp::Ordering::Equal,
            }
        });

        for cl in sorted {
            render_single_changelog(ui, cl, self);
            ui.add_space(8.0);
        }
    }
}

fn render_single_changelog(ui: &mut Ui, cl: &ChangelogVersion, app: &Foxy) {
    ui.label(
        RichText::new(format!("v{}", cl.version))
            .strong()
            .color(app.color_primary_accent()),
    );

    if !cl.date.is_empty() {
        ui.label(RichText::new(&cl.date).color(app.color_text_dim()));
    }

    for section in &cl.sections {
        ui.add_space(4.0);
        ui.label(RichText::new(&section.title).strong());
        for item in &section.items {
            ui.add(Label::new(format!("- {}", item)).wrap());
        }
    }
}
