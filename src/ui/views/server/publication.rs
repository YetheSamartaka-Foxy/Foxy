use crate::core::server::PlayerPublicationRequest;
use crate::core::server::ServerAddonRole;
use crate::ui::app::Foxy;
use crate::ui::app::server::ServerTaskAction;
use crate::ui::i18n::{tr, tr_fmt};
use eframe::egui::{self, Ui};
use std::collections::BTreeSet;

impl Foxy {
    pub(crate) fn render_server_player_publication(
        &mut self,
        ui: &mut Ui,
    ) -> Option<egui::Response> {
        if !crate::core::game::registry()
            .active()
            .server_capabilities()
            .player_publication
        {
            return None;
        }
        let editor = self.server_view.editor.as_ref()?;
        let instance_id = editor.baseline.id.clone();
        let is_arma = matches!(
            &editor.baseline.definition,
            crate::core::server::ServerDefinition::Arma3(_)
        );
        let selected_record = self
            .server_view
            .deployment_record_state
            .as_ref()
            .filter(|state| state.instance_id == instance_id)
            .and_then(|state| state.selected.clone());
        let busy = self.server_view.busy();
        let publication = &mut self.server_view.publication;
        let mut action = None;

        ui.separator();
        ui.heading(tr("Player content publication"));
        if is_arma {
            ui.label(tr("Create a separate offline repository from the selected committed deployment record."));
        } else {
            ui.label(tr(
                "Export pinned addon references from the selected committed deployment record.",
            ));
        }
        let Some(record_id) = selected_record else {
            ui.label(tr(
                "Select and verify a committed deployment record before planning player content.",
            ));
            let response =
                ui.add_enabled(!busy, egui::Button::new(tr("Refresh deployment records")));
            Self::focus_outline(ui, &response);
            if response.clicked() {
                self.start_server_task(ServerTaskAction::ListDeploymentRecords(instance_id));
            }
            return Some(response);
        };
        ui.label(tr_fmt(
            "Selected committed record: {record}",
            &[("record", record_id.clone())],
        ));

        let path_id = egui::Id::unique(("server.player-publication-output", instance_id.as_str()));
        ui.horizontal_wrapped(|ui| {
            let caption = ui.label(tr("Offline output folder"));
            let response = ui
                .add(
                    egui::TextEdit::singleline(&mut publication.output)
                        .id(path_id)
                        .desired_width(320.0)
                        .hint_text(tr("Absolute folder path")),
                )
                .labelled_by(caption.id);
            Self::focus_outline(ui, &response);
        });
        if is_arma {
            let repo_name_id =
                egui::Id::unique(("server.player-publication-repo-name", instance_id.as_str()));
            ui.horizontal_wrapped(|ui| {
                let caption = ui.label(tr("Repository name"));
                let response = ui
                    .add(
                        egui::TextEdit::singleline(&mut publication.repository_name)
                            .id(repo_name_id),
                    )
                    .labelled_by(caption.id);
                Self::focus_outline(ui, &response);
            });
            let keys = ui.checkbox(
                &mut publication.include_public_keys,
                tr("Include public addon keys"),
            );
            Self::focus_outline(ui, &keys);
        }

        let output = std::path::Path::new(publication.output.trim());
        let request = PlayerPublicationRequest {
            instance_id: instance_id.clone(),
            output: output.to_path_buf(),
            repository_name: if is_arma {
                publication.repository_name.trim().to_owned()
            } else {
                "Player content".into()
            },
            recommendations: if is_arma {
                publication.recommendations.clone()
            } else {
                Vec::new()
            },
            include_public_keys: is_arma && publication.include_public_keys,
        };
        if publication
            .plan
            .as_ref()
            .is_some_and(|plan| plan.request != request || plan.record_id != record_id)
        {
            publication.arma_permission = false;
        }
        let request_valid = output.is_absolute() && !request.repository_name.is_empty() && !busy;
        let plan_button =
            ui.add_enabled(request_valid, egui::Button::new(tr("Plan player content")));
        Self::focus_outline(ui, &plan_button);
        if plan_button.clicked() {
            publication.plan = None;
            publication.outcome = None;
            action = Some(ServerTaskAction::PlanPlayerPublication(request.clone()));
        }

        if let Some(plan) = publication
            .plan
            .as_ref()
            .filter(|plan| plan.request.instance_id == instance_id && plan.record_id == record_id)
        {
            ui.separator();
            ui.label(tr_fmt(
                "Reviewed record: {record}",
                &[("record", plan.record_id.clone())],
            ));
            ui.label(tr_fmt(
                "{game} in game space {space}",
                &[
                    ("space", plan.game_space_id.clone()),
                    (
                        "game",
                        tr(match plan.game {
                            crate::core::server::ServerDefinitionKind::Arma3 => "Arma 3",
                            crate::core::server::ServerDefinitionKind::Reforger => "Arma Reforger",
                        }),
                    ),
                ],
            ));
            ui.label(tr_fmt(
                "Files to publish {files}, bytes {bytes}",
                &[
                    ("files", plan.files.to_string()),
                    ("bytes", plan.bytes.to_string()),
                ],
            ));
            match plan.game {
                crate::core::server::ServerDefinitionKind::Arma3 => {
                    let mut displayed = BTreeSet::new();
                    for addon in &plan.arma_addons {
                        ui.label(tr_fmt("{profile} ({name}) - {role}, files {files}, bytes {bytes}, excluded {excluded}", &[
                            ("profile", addon.profile.clone()),
                            ("name", addon.name.clone()),
                            ("role", tr(match addon.role { ServerAddonRole::Required => "Shared", ServerAddonRole::Optional => "Optional", ServerAddonRole::ServerOnly => "Server only" })),
                            ("files", addon.files.to_string()),
                            ("bytes", addon.bytes.to_string()),
                            ("excluded", addon.excluded_files.to_string()),
                        ]));
                        if addon.role == ServerAddonRole::Optional
                            && displayed.insert(&addon.profile)
                        {
                            let mut selected = publication.recommendations.contains(&addon.profile);
                            let check = ui.checkbox(
                                &mut selected,
                                tr_fmt(
                                    "Recommend {profile}",
                                    &[("profile", addon.profile.clone())],
                                ),
                            );
                            Self::focus_outline(ui, &check);
                            if check.changed() {
                                publication.arma_permission = false;
                                if selected {
                                    publication.recommendations.push(addon.profile.clone());
                                } else {
                                    publication
                                        .recommendations
                                        .retain(|profile| profile != &addon.profile);
                                }
                            }
                        }
                    }
                    let permission = ui.add_enabled(
                        plan.request == request
                            && publication.recommendations == request.recommendations,
                        egui::Checkbox::new(
                            &mut publication.arma_permission,
                            tr("I confirm I have permission to redistribute these Arma addons."),
                        ),
                    );
                    Self::focus_outline(ui, &permission);
                }
                crate::core::server::ServerDefinitionKind::Reforger => {
                    if let Some(review) = &plan.reforger {
                        ui.label(tr_fmt(
                            "Scenario {id}, provider {provider}",
                            &[
                                ("id", review.scenario_id.clone()),
                                (
                                    "provider",
                                    review
                                        .scenario_provider
                                        .clone()
                                        .unwrap_or_else(|| tr("Unknown")),
                                ),
                            ],
                        ));
                        for addon in &review.addons {
                            ui.label(tr_fmt("Required addon {id}, version {version}, provider {provider}", &[
                                ("id", addon.id.clone()),
                                ("version", addon.version.clone()),
                                ("provider", tr(match addon.provider {
                                    crate::core::server::ReforgerAddonProvider::Local => "Local",
                                    crate::core::server::ReforgerAddonProvider::BohemiaWorkshop => "Bohemia Workshop",
                                })),
                            ]));
                        }
                        for omission in &review.omissions {
                            ui.label(tr_fmt(
                                "Omitted optional addon {id}",
                                &[("id", omission.clone())],
                            ));
                        }
                    }
                }
            }
            if plan.destination_exists {
                ui.label(tr("The output folder already exists. Verify can inspect this reviewed destination."));
            }
            let authorized = plan.game != crate::core::server::ServerDefinitionKind::Arma3
                || publication.arma_permission;
            let inputs_current =
                plan.request == request && publication.recommendations == request.recommendations;
            if !inputs_current {
                ui.label(tr(
                    "Publication settings changed. Plan again to review the updated selection.",
                ));
            }
            let publish = ui.add_enabled(
                !busy
                    && authorized
                    && inputs_current
                    && !plan.destination_exists
                    && publication.outcome.is_none(),
                egui::Button::new(tr("Publish player content")),
            );
            Self::focus_outline(ui, &publish);
            if publish.clicked() {
                action = Some(ServerTaskAction::ApplyPlayerPublication(Box::new(
                    plan.clone(),
                )));
            }
            let verify = ui.add_enabled(
                !busy
                    && inputs_current
                    && (plan.destination_exists || publication.outcome.is_some()),
                egui::Button::new(tr("Verify published player content")),
            );
            Self::focus_outline(ui, &verify);
            if verify.clicked() {
                action = Some(ServerTaskAction::VerifyPlayerPublication(Box::new(
                    plan.clone(),
                )));
            }
        }
        if let Some(outcome) = publication.outcome.as_ref().filter(|outcome| {
            outcome.record_id == record_id
                && publication
                    .plan
                    .as_ref()
                    .is_some_and(|plan| plan.request == request)
        }) {
            ui.label(tr_fmt(
                if outcome.writes_performed {
                    "Published verified player content to {path}."
                } else {
                    "Verified player content in {path}."
                },
                &[("path", outcome.output.display().to_string())],
            ));
        }
        if let Some(action) = action {
            self.start_server_task(action);
        }
        Some(plan_button)
    }
}
