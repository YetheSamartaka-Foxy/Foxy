use crate::core::server::{InstallationRecoveryInspection, RecoveryMetadataEvidence};
use crate::ui::app::Foxy;
use eframe::egui::{self, RichText, Ui};

impl Foxy {
    pub(super) fn render_server_recovery(
        &self,
        ui: &mut Ui,
        inspection: &InstallationRecoveryInspection,
    ) {
        ui.separator();
        ui.heading(self.t("Recovery and admission review"));
        ui.label(self.t(
            "This metadata-only report does not verify archives, process liveness, or engine admission.",
        ));
        ui.label(self.t("Foreign transaction evidence is not inspected in this report."));

        let maintenance = if inspection.maintenance_status == "pending" {
            self.t("Pending recovery")
        } else if inspection.maintenance_status == "clear" && inspection.inspection_complete {
            self.t("No pending recovery intent")
        } else {
            self.t("Unknown")
        };
        ui.label(self.t_fmt("Maintenance intent: {status}", &[("status", maintenance)]));

        let consumer_status = if inspection.consumers.inspection_complete {
            self.t_fmt(
                "Registered consumer records: {count}",
                &[(
                    "count",
                    inspection.consumers.recorded_consumers.len().to_string(),
                )],
            )
        } else {
            self.t("Registered consumer records: unknown")
        };
        ui.label(consumer_status);
        let receipt_count = if inspection.inspection_complete {
            inspection
                .receipt_count
                .map(|count| count.to_string())
                .unwrap_or_else(|| self.t("unknown"))
        } else {
            self.t("unknown")
        };
        ui.label(self.t_fmt("Receipt history: {count}", &[("count", receipt_count)]));
        ui.label(
            self.t_fmt(
                "Receipt retention limit reached: {status}",
                &[(
                    "status",
                    option_status(
                        self,
                        inspection
                            .inspection_complete
                            .then_some(inspection.retention_limit_reached)
                            .flatten(),
                    ),
                )],
            ),
        );
        if inspection.evidence_truncated {
            ui.label(self.t("Older recovery history is truncated."));
        }

        ui.label(self.t_fmt(
            "Native runtime layout: {status}",
            &[("status", gate_status(self, inspection.native_layout))],
        ));
        ui.label(self.t_fmt(
            "Stopped process ownership: {status}",
            &[("status", gate_status(self, inspection.stopped_owner))],
        ));
        ui.label(self.t_fmt(
            "Content trust: {status}",
            &[("status", gate_status(self, inspection.content_trust))],
        ));
        ui.label(self.t_fmt(
            "Owner handoff: {status}",
            &[("status", gate_status(self, inspection.owner_handoff))],
        ));

        if let Some(error) = inspection.inspection_error {
            ui.label(self.t("Recovery inspection is incomplete; status is unknown."));
            ui.label(self.t_fmt(
                "Inspection detail: {detail}",
                &[("detail", error.to_owned())],
            ));
        }
        if let Some(error) = inspection.consumers.inspection_error {
            ui.label(self.t(error));
        }

        let id = ui.make_persistent_id((
            "server-recovery-evidence",
            crate::core::game::spaces::active_game_space().space_id,
            inspection.installation_id.as_str(),
        ));
        let default_open = inspection
            .evidence
            .iter()
            .any(|evidence| evidence.pending || evidence.inspection_error.is_some());
        let mut open = ui
            .ctx()
            .data_mut(|data| data.get_temp::<bool>(id))
            .unwrap_or(default_open);
        let response = ui
            .push_id(id, |ui| {
                ui.toggle_value(
                    &mut open,
                    self.t_fmt(
                        "Recovery evidence ({count})",
                        &[("count", inspection.evidence.len().to_string())],
                    ),
                )
            })
            .inner;
        if response.clicked() {
            response.request_focus();
        }
        if response.has_focus() {
            ui.input_mut(|input| {
                input.consume_key(egui::Modifiers::NONE, egui::Key::Enter);
                input.consume_key(egui::Modifiers::NONE, egui::Key::Space);
            });
        }
        ui.ctx().data_mut(|data| data.insert_temp(id, open));
        if open {
            ui.indent(id.with("details"), |ui| {
                for (index, evidence) in inspection.evidence.iter().take(17).enumerate() {
                    ui.push_id(("recovery-evidence-row", index), |ui| {
                        render_evidence(self, ui, evidence);
                    });
                }
            });
        }
    }
}

fn render_evidence(foxy: &Foxy, ui: &mut Ui, evidence: &RecoveryMetadataEvidence) {
    ui.separator();
    ui.label(foxy.t_fmt("Instance: {id}", &[("id", evidence.instance_id.clone())]));
    ui.label(foxy.t(if evidence.pending {
        "Pending maintenance intent"
    } else {
        "Historical maintenance intent"
    }));
    ui.label(foxy.t("Intent fingerprint"));
    ui.add(
        egui::Label::new(RichText::new(&evidence.intent_fingerprint).monospace()).selectable(true),
    );
    ui.label(foxy.t("Transaction fingerprint"));
    ui.add(
        egui::Label::new(RichText::new(&evidence.transaction_fingerprint).monospace())
            .selectable(true),
    );
    ui.label(foxy.t_fmt(
        "Context evidence: {status}",
        &[(
            "status",
            evidence_status(foxy, "context", evidence.context_status),
        )],
    ));
    ui.label(foxy.t_fmt(
        "Journal evidence: {status}",
        &[(
            "status",
            evidence_status(foxy, "journal", evidence.journal_status),
        )],
    ));
    ui.label(foxy.t_fmt(
        "Active endpoint: {status}",
        &[(
            "status",
            evidence_status(foxy, "endpoint", evidence.active_endpoint),
        )],
    ));
    ui.label(foxy.t_fmt(
        "Resolution evidence: {status}",
        &[(
            "status",
            evidence_status(foxy, "resolution", evidence.resolution_status),
        )],
    ));
    ui.label(foxy.t_fmt(
        "Receipt evidence: {status}",
        &[(
            "status",
            evidence_status(foxy, "receipt", evidence.receipt_status),
        )],
    ));
    ui.label(foxy.t_fmt(
        "Catalog unchanged: {status}",
        &[("status", option_status(foxy, evidence.catalog_unchanged))],
    ));
    ui.label(foxy.t_fmt(
        "Selection unchanged: {status}",
        &[("status", option_status(foxy, evidence.selection_unchanged))],
    ));
    if let Some(error) = evidence.inspection_error {
        ui.label(foxy.t("Evidence inspection is incomplete."));
        ui.label(foxy.t_fmt(
            "Inspection detail: {detail}",
            &[("detail", error.to_owned())],
        ));
    }
}

fn option_status(foxy: &Foxy, value: Option<bool>) -> String {
    match value {
        Some(true) => foxy.t("Yes"),
        Some(false) => foxy.t("No"),
        None => foxy.t("Unknown"),
    }
}

fn gate_status(foxy: &Foxy, value: &str) -> String {
    match value {
        "unsupported" => foxy.t("Unsupported"),
        "unverified" => foxy.t("Unverified"),
        "not_implemented" => foxy.t("Not implemented"),
        "verified" => foxy.t("Verified"),
        _ => foxy.t("Unknown"),
    }
}

fn evidence_status(foxy: &Foxy, field: &str, value: &str) -> String {
    match (field, value) {
        ("context", "local_context") => foxy.t("Local context"),
        ("context", "foreign_context") => foxy.t("Foreign context"),
        ("journal", "not_inspected") => foxy.t("Not inspected"),
        ("journal", "absent") => foxy.t("No journal found"),
        ("journal", "matches") => foxy.t("Journal matches evidence"),
        ("journal", "mismatch") => foxy.t("Journal does not match evidence"),
        ("journal", "different_transaction") => foxy.t("Different transaction"),
        ("endpoint", "not_inspected") => foxy.t("Not inspected"),
        ("endpoint", "predecessor") => foxy.t("Predecessor retained"),
        ("endpoint", "candidate") => foxy.t("Candidate endpoint matches intent"),
        ("endpoint", "other_generation") => foxy.t("Other generation; ambiguous"),
        ("resolution", "not_inspected") => foxy.t("Not inspected"),
        ("resolution", "absent") => foxy.t("No resolution record"),
        ("resolution", "matches") => foxy.t("Resolution matches evidence"),
        ("receipt", "not_inspected") => foxy.t("Not inspected"),
        ("receipt", "absent") => foxy.t("No receipt"),
        ("receipt", "recorded") => foxy.t("Receipt recorded"),
        ("receipt", "matches") => foxy.t("Receipt matches evidence"),
        _ => foxy.t("Unknown"),
    }
}
