use crate::core::server::{
    WorkshopBatchStatus, WorkshopItemAction, WorkshopItemStatus, WorkshopRequest,
};
use crate::ui::app::Foxy;
use crate::ui::app::server::{ServerDialog, ServerTaskAction};
use crate::ui::i18n::{tr, tr_fmt};
use eframe::egui::{self, Ui};

fn action_text(action: WorkshopItemAction) -> &'static str {
    match action {
        WorkshopItemAction::Acquire => "Download and pin",
        WorkshopItemAction::Update => "Update the pinned revision",
        WorkshopItemAction::Repair => "Repair the pinned revision",
        WorkshopItemAction::Current => "Pinned revision is current",
        WorkshopItemAction::Unavailable => "Not downloadable",
    }
}

fn status_text(status: WorkshopItemStatus) -> &'static str {
    match status {
        WorkshopItemStatus::Acquired => "Downloaded, verified and pinned",
        WorkshopItemStatus::Reused => "Existing verified revision pinned",
        WorkshopItemStatus::Current => "Already current",
        WorkshopItemStatus::Unavailable => "Not downloadable",
        WorkshopItemStatus::Failed => "Failed; nothing changed for this item",
        WorkshopItemStatus::Cancelled => "Cancelled; nothing changed for this item",
    }
}

impl Foxy {
    pub(crate) fn render_server_workshop(&mut self, ui: &mut Ui) -> Option<egui::Response> {
        if !crate::core::game::registry()
            .active()
            .server_capabilities()
            .workshop_acquisition
        {
            return None;
        }
        let busy = self.server_view.busy();
        let error_color = self.color_text_error();
        let alias = self.server_view.steam_auth.alias.trim().to_owned();
        let mut action = None;
        let mut confirm = false;
        ui.separator();
        let anchor = ui.push_id("server.workshop-controls", |ui| {
            ui.heading(tr("Workshop items for servers"));
            ui.label(tr("Download Arma 3 Workshop items with a prepared native Steam session into verified revisions. Each pinned item becomes the already-present content profile workshop-<id> for content bindings."));
            let state = &mut self.server_view;
            ui.horizontal_wrapped(|ui| {
                let label = ui.label(tr("Workshop item ids or links"));
                if ui
                    .add(egui::TextEdit::singleline(&mut state.workshop_items_input).desired_width(420.0))
                    .labelled_by(label.id)
                    .changed()
                {
                    state.workshop_plan = None;
                }
            });
            ui.horizontal_wrapped(|ui| {
                if ui
                    .checkbox(&mut state.workshop_anonymous, tr("Use an anonymous Steam connection"))
                    .changed()
                    | ui.checkbox(&mut state.workshop_repair, tr("Repair the pinned revision of tracked items"))
                        .changed()
                {
                    state.workshop_plan = None;
                }
            });
            ui.label(if state.workshop_anonymous {
                tr("Anonymous connections can review items but cannot download Arma 3 Workshop content.")
            } else {
                tr_fmt("Prepared Steam session alias: {alias}", &[("alias", alias.clone())])
            });
            let items: Vec<String> = crate::core::game::workshop::parse_workshop_item_ids(&state.workshop_items_input);
            let review = ui.add_enabled(!busy && !items.is_empty(), egui::Button::new(tr("Review Workshop items")));
            if review.clicked() {
                action = Some(ServerTaskAction::PlanWorkshop(WorkshopRequest {
                    items,
                    account: (!state.workshop_anonymous).then(|| alias.clone()),
                    repair: state.workshop_repair,
                }));
            }
            if let Some(plan) = &state.workshop_plan {
                egui::CollapsingHeader::new(tr("Reviewed Workshop items"))
                    .default_open(true)
                    .show(ui, |ui| {
                        for item in &plan.items {
                            ui.label(tr_fmt(
                                "{id} {title}: {action}; manifest {manifest}; {bytes} bytes",
                                &[
                                    ("id", item.item_id.clone()),
                                    ("title", item.title.clone()),
                                    ("action", tr(action_text(item.action))),
                                    ("manifest", item.manifest_gid.clone().unwrap_or_else(|| tr("None"))),
                                    ("bytes", item.file_size.to_string()),
                                ],
                            ));
                            if let Some(issue) = item.issue {
                                ui.colored_label(error_color, tr(issue.message()));
                            }
                        }
                        ui.label(if plan.entitled {
                            tr("The Steam session can download Arma 3 Workshop content.")
                        } else {
                            tr("The Steam session cannot download Arma 3 Workshop content.")
                        });
                        for blocker in &plan.blockers {
                            ui.colored_label(error_color, tr(blocker));
                        }
                        ui.label(tr_fmt("Review fingerprint: {fingerprint}", &[("fingerprint", plan.fingerprint.clone())]));
                    });
                confirm = ui
                    .add_enabled(!busy && plan.blockers.is_empty(), egui::Button::new(tr("Acquire reviewed items")))
                    .clicked();
            }
            if let Some(outcome) = &state.workshop_outcome {
                ui.label(tr(match outcome.status {
                    WorkshopBatchStatus::Complete => "Every reviewed Workshop item was verified and pinned.",
                    WorkshopBatchStatus::Partial => "Some Workshop items were pinned; others failed. Review the item results.",
                    WorkshopBatchStatus::Failed => "No Workshop item was pinned. Review the item results.",
                    WorkshopBatchStatus::Cancelled => "Workshop acquisition cancelled; finished items stay pinned.",
                }));
                for item in &outcome.items {
                    let line = tr_fmt(
                        "{id}: {status}",
                        &[("id", item.item_id.clone()), ("status", tr(status_text(item.status)))],
                    );
                    if matches!(item.status, WorkshopItemStatus::Failed | WorkshopItemStatus::Cancelled) {
                        ui.colored_label(error_color, line);
                    } else {
                        ui.label(line);
                    }
                    if let Some(error) = item.content_error {
                        ui.colored_label(error_color, tr(&error.to_string()));
                    }
                    if let Some(error) = &item.error {
                        ui.colored_label(error_color, tr(error));
                    }
                }
            }
            let refresh = ui.add_enabled(!busy, egui::Button::new(tr("Refresh tracked Workshop items")));
            if refresh.clicked() {
                action = Some(ServerTaskAction::LoadWorkshop);
            }
            if let Some(snapshot) = &state.workshop_snapshot {
                if snapshot.items.is_empty() {
                    ui.label(tr("No tracked Workshop items in this game space."));
                }
                for item in snapshot.items.iter().take(256) {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(tr_fmt(
                            "{id} {title}: pinned manifest {manifest}; profile {profile}",
                            &[
                                ("id", item.item_id.clone()),
                                ("title", item.title.clone()),
                                ("manifest", item.pinned_manifest.clone()),
                                ("profile", item.profile.clone()),
                            ],
                        ));
                        if ui.add_enabled(!busy, egui::Button::new(tr("Verify pinned revision"))).clicked() {
                            action = Some(ServerTaskAction::VerifyWorkshop(item.item_id.clone()));
                        }
                    });
                }
            }
            if let Some(report) = &state.workshop_verification {
                if report.verified {
                    ui.label(tr_fmt(
                        "Item {id} verified: {files} file(s), {bytes} bytes.",
                        &[("id", report.item_id.clone()), ("files", report.files.to_string()), ("bytes", report.bytes.to_string())],
                    ));
                } else {
                    ui.colored_label(
                        error_color,
                        tr_fmt("Item {id} does not match its recorded revision. Review it with repair selected.", &[("id", report.item_id.clone())]),
                    );
                }
            }
            review
        });
        if confirm {
            self.server_view.open_dialog(ServerDialog::AcquireWorkshop);
        }
        if let Some(action) = action {
            self.start_server_task(action);
        }
        Some(anchor.inner)
    }
}
