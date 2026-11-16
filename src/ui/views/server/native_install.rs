use crate::core::server::{
    BuildEvidence, InstallationOwnership, NativeInstallRequest, NativeInstallStatus,
    NativeMaintenanceAction,
};
use crate::ui::app::Foxy;
use crate::ui::app::server::{ServerDialog, ServerTaskAction};
use crate::ui::i18n::{tr, tr_fmt};
use eframe::egui::{self, Ui};

impl Foxy {
    pub(crate) fn render_server_native_install(&mut self, ui: &mut Ui) -> Option<egui::Response> {
        if !crate::core::game::registry()
            .active()
            .server_capabilities()
            .native_installation
        {
            return None;
        }
        let editor = self.server_view.editor.as_ref()?;
        let id = editor.baseline.id.clone();
        let directory_id = editor.field_scope().with("native-install-directory");
        let installation = editor
            .baseline
            .installation_id
            .as_ref()
            .and_then(|installation_id| {
                self.server_view
                    .catalog
                    .as_ref()?
                    .installations
                    .iter()
                    .find(|installation| {
                        &installation.id == installation_id
                            && installation.ownership == InstallationOwnership::Managed
                            && installation.build_evidence == BuildEvidence::NativeDepotManifest
                    })
            });
        let managed_installation = installation.is_some();
        let channel_selectable = crate::core::game::registry()
            .active()
            .server_capabilities()
            .native_installation_channels;
        let dirty = editor.dirty();
        let busy = self.server_view.busy();
        let error_color = self.color_text_error();
        if self
            .server_view
            .native_install_plan
            .as_ref()
            .is_some_and(|plan| plan.instance_id != id)
        {
            self.server_view.native_install_plan = None;
        }
        let alias = self.server_view.steam_auth.alias.trim().to_owned();
        let mut action = None;
        let mut confirm = false;
        ui.separator();
        let anchor = ui.push_id("server.native-install-controls", |ui| {
            ui.heading(tr("Managed installation from Steam"));
            ui.label(tr("Download the dedicated server with a prepared native Steam session into a new folder. Every file is verified against the depot manifests before the folder is published and bound to this instance."));
            let state = &mut self.server_view;
            let mut maintenance_anchor = None;
            ui.horizontal_wrapped(|ui| {
                let label = ui.label(tr("New installation folder"));
                if ui
                    .add(egui::TextEdit::singleline(&mut state.native_install_directory).id(directory_id).desired_width(350.0))
                    .labelled_by(label.id)
                    .changed()
                {
                    state.native_install_plan = None;
                }
            });
            ui.horizontal_wrapped(|ui| {
                let label = ui.label(tr("Steam branch"));
                if ui
                    .add(egui::TextEdit::singleline(&mut state.native_install_branch).hint_text("public").desired_width(160.0))
                    .labelled_by(label.id)
                    .changed()
                {
                    state.native_install_plan = None;
                    state.native_install_check = None;
                }
                if ui
                    .checkbox(&mut state.native_install_anonymous, tr("Use an anonymous Steam connection"))
                    .changed()
                {
                    state.native_install_plan = None;
                    state.native_install_check = None;
                }
            });
            if channel_selectable {
                ui.horizontal_wrapped(|ui| {
                    let label = ui.label(tr("New installation channel"));
                    let previous = state.native_install_channel;
                    let selector = egui::ComboBox::from_id_salt("server-native-install-channel")
                        .selected_text(match state.native_install_channel {
                            crate::core::server::ContentChannel::Stable => tr("Stable"),
                            crate::core::server::ContentChannel::Experimental => tr("Experimental"),
                        })
                        .show_ui(ui, |ui| {
                            let stable = ui.selectable_value(&mut state.native_install_channel, crate::core::server::ContentChannel::Stable, tr("Stable"));
                            let experimental = ui.selectable_value(&mut state.native_install_channel, crate::core::server::ContentChannel::Experimental, tr("Experimental"));
                            if stable.clicked() || experimental.clicked() {
                                ui.close();
                            }
                        });
                    selector.response.labelled_by(label.id);
                    if previous != state.native_install_channel {
                        state.native_install_plan = None;
                        state.native_install_check = None;
                        state.native_install_outcome = None;
                    }
                });
                ui.label(tr("This channel applies to a new installation. Check, update, and repair keep the bound installation's channel. Steam branch is a separate choice within the selected app."));
            }
            ui.label(if state.native_install_anonymous {
                tr("Anonymous connections can review the build but cannot download apps that require ownership.")
            } else {
                tr_fmt("Prepared Steam session alias: {alias}", &[("alias", alias.clone())])
            });
            if managed_installation {
                ui.horizontal_wrapped(|ui| {
                    let check = ui.add_enabled(!busy && !dirty, egui::Button::new(tr("Check updates")));
                    if check.clicked() {
                        state.native_install_check = None;
                        state.native_install_validation = None;
                        action = Some(ServerTaskAction::CheckNativeInstallation {
                            id: id.clone(),
                            account: (!state.native_install_anonymous).then(|| alias.clone()),
                        });
                    }
                    maintenance_anchor = Some(check);
                    if ui.add_enabled(!busy && !dirty, egui::Button::new(tr("Validate installed files"))).clicked() {
                        state.native_install_validation = None;
                        action = Some(ServerTaskAction::ValidateNativeInstallation(id.clone()));
                    }
                    if ui.add_enabled(!busy && !dirty && !state.native_install_directory.trim().is_empty(), egui::Button::new(tr("Review update"))).clicked() {
                        action = Some(ServerTaskAction::PlanNativeMaintenance {
                            request: native_install_request(state, &id, &alias),
                            action: NativeMaintenanceAction::Update,
                        });
                    }
                    if ui.add_enabled(!busy && !dirty && !state.native_install_directory.trim().is_empty(), egui::Button::new(tr("Review repair"))).clicked() {
                        action = Some(ServerTaskAction::PlanNativeMaintenance {
                            request: native_install_request(state, &id, &alias),
                            action: NativeMaintenanceAction::Repair,
                        });
                    }
                });
                ui.label(tr("Choose a new folder that does not exist. Update and repair create a fresh installation and bind it to this instance only. The old installation root, unknown files, and other instances are retained. Foxy does not restart the server automatically."));
            }
            if let Some(check) = &state.native_install_check {
                ui.label(tr_fmt(
                    "Installed build: {build}; {availability}",
                    &[
                        ("build", check.installed_build_id.map_or_else(|| tr("Unknown"), |build| build.to_string())),
                        ("availability", if check.update_available { tr("Update available") } else { tr("No update available") }),
                    ],
                ));
                ui.label(tr_fmt("Installed channel: {channel}", &[("channel", match check.channel {
                    crate::core::server::ContentChannel::Stable => tr("Stable"),
                    crate::core::server::ContentChannel::Experimental => tr("Experimental"),
                })]));
                ui.label(tr_fmt("Latest reviewed build: {build}", &[ ("build", check.review.selection.build_id.map_or_else(|| tr("Unknown"), |build| build.to_string())) ]));
            }
            if let Some(validation) = &state.native_install_validation {
                ui.label(if validation.valid { tr("Managed installation files match the recorded verified inventory.") } else { tr("Managed installation files do not match the recorded verified inventory.") });
                ui.label(tr_fmt("Validated files: {files}; bytes: {bytes}", &[("files", validation.files.to_string()), ("bytes", validation.bytes.to_string())]));
                if let Some(error) = &validation.error {
                    ui.colored_label(error_color, tr(error));
                }
            }
            let directory = state.native_install_directory.trim().to_owned();
            let review = ui.add_enabled(
                !busy && !dirty && !directory.is_empty(),
                egui::Button::new(tr("Review native installation")),
            );
            if review.clicked() {
                let branch = state.native_install_branch.trim();
                action = Some(ServerTaskAction::PlanNativeInstall(NativeInstallRequest {
                    instance_id: id.clone(),
                    directory: directory.into(),
                    branch: if branch.is_empty() { "public".into() } else { branch.to_owned() },
                    account: (!state.native_install_anonymous).then(|| alias.clone()),
                    channel: channel_selectable.then_some(state.native_install_channel),
                }));
            }
            if dirty {
                ui.label(tr("Save or discard draft changes before reviewing a native installation."));
            }
            if let Some(plan) = &state.native_install_plan {
                egui::CollapsingHeader::new(tr("Reviewed Steam build"))
                    .default_open(true)
                    .show(ui, |ui| {
                        let selection = &plan.review.selection;
                        ui.label(tr_fmt(
                            "Steam app {app}, branch {branch}, build {build}",
                            &[
                                ("app", plan.app_id.to_string()),
                                ("branch", plan.branch.clone()),
                                ("build", selection.build_id.map_or_else(|| tr("Unknown"), |build| build.to_string())),
                            ],
                        ));
                        ui.label(tr_fmt("Reviewed channel: {channel}", &[("channel", match plan.channel {
                            crate::core::server::ContentChannel::Stable => tr("Stable"),
                            crate::core::server::ContentChannel::Experimental => tr("Experimental"),
                        })]));
                        ui.label(if plan.review.entitled {
                            tr("The Steam session can download every selected depot.")
                        } else {
                            tr("The Steam session is not entitled to these depots.")
                        });
                        ui.label(tr_fmt("New installation folder: {path}", &[("path", plan.directory.display().to_string())]));
                        ui.label(tr_fmt(
                            "Download size: {required} bytes; free space: {available} bytes",
                            &[
                                ("required", plan.required_bytes.to_string()),
                                ("available", plan.available_bytes.map_or_else(|| tr("Unknown"), |bytes| bytes.to_string())),
                            ],
                        ));
                        for depot in &selection.depots {
                            ui.label(tr_fmt(
                                "Depot {depot}, manifest {manifest}, {bytes} bytes",
                                &[
                                    ("depot", depot.depot_id.to_string()),
                                    ("manifest", depot.manifest_gid.to_string()),
                                    ("bytes", depot.size.map_or_else(|| tr("Unknown"), |size| size.to_string())),
                                ],
                            ));
                        }
                        ui.label(tr_fmt("Depots skipped for this platform or branch: {count}", &[("count", selection.skipped.len().to_string())]));
                        if !plan.interrupted_staging.is_empty() {
                            ui.label(tr_fmt(
                                "Interrupted Foxy staging folders to remove first: {count}",
                                &[("count", plan.interrupted_staging.len().to_string())],
                            ));
                        }
                        for blocker in &plan.blockers {
                            ui.colored_label(error_color, tr(blocker));
                        }
                        ui.label(tr_fmt("Review fingerprint: {fingerprint}", &[("fingerprint", plan.fingerprint.clone())]));
                    });
                let confirmation_label = match plan.maintenance.as_ref().map(|review| &review.action) {
                    Some(NativeMaintenanceAction::Update) => tr("Confirm reviewed update"),
                    Some(NativeMaintenanceAction::Repair) => tr("Confirm reviewed repair"),
                    None => tr("Install reviewed build"),
                };
                confirm = ui
                    .add_enabled(!busy && plan.blockers.is_empty(), egui::Button::new(confirmation_label))
                    .clicked();
            }
            if let Some(outcome) = &state.native_install_outcome {
                ui.label(match outcome.status {
                    NativeInstallStatus::Installed => tr("Managed installation verified, published and bound."),
                    NativeInstallStatus::PublishedUnbound => tr("Verified installation published but not bound."),
                    NativeInstallStatus::Cancelled => tr("Native installation cancelled; the destination was not created."),
                    NativeInstallStatus::Failed => tr("Native installation failed; the destination was not created."),
                });
                if outcome.published {
                    ui.label(tr_fmt(
                        "Files: {files}; bytes: {bytes}; folder: {path}",
                        &[
                            ("files", outcome.files.to_string()),
                            ("bytes", outcome.bytes.to_string()),
                            ("path", outcome.directory.display().to_string()),
                        ],
                    ));
                }
                if let Some(error) = outcome.content_error {
                    ui.colored_label(error_color, tr(&error.to_string()));
                }
                if let Some(error) = outcome.auth_error {
                    ui.colored_label(error_color, tr(&error.to_string()));
                }
                if let Some(error) = &outcome.error {
                    ui.colored_label(error_color, tr(error));
                }
                if outcome.staging_removed == Some(false) {
                    ui.colored_label(error_color, tr("The staging folder could not be removed; review it again to clean it up."));
                }
            }
            maintenance_anchor.unwrap_or(review)
        });
        if confirm {
            self.server_view.dialog = Some(ServerDialog::NativeInstall);
        }
        if let Some(action) = action {
            self.start_server_task(action);
        }
        Some(anchor.inner)
    }
}

fn native_install_request(
    state: &crate::ui::app::server::ServerViewState,
    id: &crate::core::server::ServerInstanceId,
    alias: &str,
) -> NativeInstallRequest {
    let branch = state.native_install_branch.trim();
    NativeInstallRequest {
        instance_id: id.clone(),
        directory: state.native_install_directory.trim().into(),
        branch: if branch.is_empty() {
            "public".into()
        } else {
            branch.to_owned()
        },
        account: (!state.native_install_anonymous).then(|| alias.to_owned()),
        channel: None,
    }
}
