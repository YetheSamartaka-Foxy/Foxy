mod configuration;
mod consumer_recovery;
mod content;
mod content_acquisition;
mod content_stage;
mod deployment_activation;
mod deployment_composition;
mod deployment_preparation;
mod deployment_records;
mod dialogs;
mod editor;
mod headless;
mod import_export;
mod installation;
mod keys;
mod lifecycle;
mod maintenance;
mod missions;
mod monitoring;
mod native_install;
mod reconciliation;
mod recovery;
mod reforger_addons;
mod steam_auth;
mod workshop;

use crate::core::game::spaces;
use crate::core::server::DraftChange;
use crate::ui::app::Foxy;
use crate::ui::app::server::ServerDialog;
use eframe::egui::{self, RichText, Ui};

impl Foxy {
    pub(crate) fn render_server_overview(&mut self, ui: &mut Ui) {
        let page_delta =
            if self.server_view.dialog.is_none() && !self.server_view.reforger_addon_confirmation {
                ui.input_mut(|input| {
                    if input.consume_key(egui::Modifiers::NONE, egui::Key::PageDown) {
                        -1.0
                    } else if input.consume_key(egui::Modifiers::NONE, egui::Key::PageUp) {
                        1.0
                    } else {
                        0.0
                    }
                }) * ui.clip_rect().height()
                    * 0.75
            } else {
                0.0
            };
        let space = spaces::active_game_space();
        let new_name_id = self.server_draft_focus_id("server-new-name").unwrap();
        let supported = self.server_drafts_supported();
        if supported && !self.server_view.loaded && !self.server_view.busy() {
            self.start_server_draft_task(None);
        }
        egui::Frame::NONE.inner_margin(15.0).show(ui, |ui| {
            egui::ScrollArea::vertical().id_salt("server-page").show(ui, |ui| {
            ui.heading(RichText::new(self.t("Server instances")).size(self.settings_view_state.font_sizes.settings_view.page_title as f32));
            ui.label(self.t_fmt("Game space: {name}", &[("name", space.display_name)]));
            ui.label(self.t(if crate::core::game::registry().get(&space.game_id).is_some_and(|module| module.server_capabilities().definition_kind == Some(crate::core::server::ServerDefinitionKind::Reforger)) {
                "Review and activate prepared native deployments, then control their foreground server sessions. Reforger configuration can preserve extra server.json fields and an imported scenario listing. Addon acquisition is separate, and a scenario name does not prove the file exists."
            } else if crate::core::game::registry().get(&space.game_id).is_some_and(|module| module.server_capabilities().lifecycle) {
                "Review and activate prepared Arma deployments, then control their foreground server sessions. Managed installs and Workshop items download with a prepared native Steam session."
            } else if crate::core::game::registry().get(&space.game_id).is_some_and(|module| module.server_capabilities().deployment_activation) {
                "Review and activate prepared Arma deployments in isolated runtime copies. Downloads and server launch are not available yet."
            } else if supported {
                "Server drafts and installation adoption are work in progress. Downloads, deployment and launch are not available yet."
            } else {
                "Dedicated server management is unavailable for this game."
            }));
            self.render_owner_status(ui);
            if !self.mode_ownership.ready() { return; }
            ui.horizontal_wrapped(|ui| {
                if ui.button(self.t("Game spaces")).clicked() { self.open_game_spaces_view(); }
                if ui.button(self.t("Application settings")).clicked() { self.open_settings_view(); self.settings_view_state.current_tab = "Application".into(); }
                if self.update_modal_open && ui.button(self.t("Client update progress")).clicked() { self.server_view.show_client_progress = true; }
            });
            if !supported { return; }
            self.render_server_steam_auth(ui);
            ui.separator();
            if self.server_view.task.as_ref().is_some_and(|task| !task.passive && task.kind.is_none()) {
                ui.horizontal(|ui| {
                    ui.add(crate::ui::app::PacedSpinner::new());
                    ui.label(self.t("Reading or saving server drafts..."));
                });
            }
            if let Some(error) = &self.server_view.error {
                ui.colored_label(self.color_text_error(), self.t(&error.to_string()));
                ui.label(self.t("Saved data and entered changes are retained. Fix the cause, or discard local changes and reload."));
            }
            if let Some(status) = self.server_view.status { ui.label(self.t(status)); }
            self.render_server_content_stage_status(ui);
            if self.server_view.migration_pending { ui.label(self.t("Legacy drafts are shown in the current schema. The next catalog save preserves a v1 backup and upgrades the catalog.")); }
            let busy = self.server_view.controls_busy() || self.pending_game_space_switch.is_some();
            let dirty = self.server_view.dirty();
            if dirty { ui.label(self.t("Save or discard draft changes before selecting another instance, reloading, or switching game spaces.")); }
            ui.add_enabled_ui(!busy && !dirty, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.label(self.t("New instance name"));
                    ui.add(egui::TextEdit::singleline(&mut self.server_view.new_name).id(new_name_id).desired_width(210.0));
                    if ui.button(self.t("Create draft")).clicked() {
                        self.start_server_draft_task(Some(DraftChange::Add { name: self.server_view.new_name.clone() }));
                    }
                    if ui.button(self.t("Reload drafts")).clicked() { self.start_server_draft_task(None); }
                });
            });
            let rows = self.server_view.catalog.as_ref().map(|catalog| catalog.instances.iter().map(|instance| (instance.id.clone(), instance.display_name.clone(), instance.revision)).collect::<Vec<_>>()).unwrap_or_default();
            let mut selected = None;
            let mut focus_target = None;
            let mut row_focus_ids = Vec::new();
            egui::ScrollArea::vertical().id_salt("server-instance-list").max_height(160.0).show(ui, |ui| {
                ui.add_enabled_ui(!busy && !dirty, |ui| {
                    for (index, (id, name, revision)) in rows.iter().enumerate() {
                        ui.push_id(("server-row", id.as_str()), |ui| {
                            let text = format!("{}  [{}]", name, revision);
                            let response = ui.selectable_label(self.server_view.selected.as_ref() == Some(id), text).on_hover_text(id.as_str());
                            row_focus_ids.push(response.id);
                            if response.clicked() { selected = Some(id.clone()); }
                            if response.has_focus() {
                                ui.memory_mut(|memory| memory.set_focus_lock_filter(response.id, egui::EventFilter { vertical_arrows: true, ..Default::default() }));
                                let offset = ui.input_mut(|input| {
                                    if input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown) { 1 }
                                    else if input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp) { -1 } else { 0 }
                                });
                                if offset != 0 {
                                    let next = (index as isize + offset).clamp(0, rows.len() as isize - 1) as usize;
                                    selected = Some(rows[next].0.clone());
                                    focus_target = Some(next);
                                }
                            }
                        });
                    }
                });
                if rows.is_empty() && !self.server_view.busy() { ui.label(self.t("No server drafts in this game space.")); }
            });
            if let Some(index) = focus_target {
                ui.memory_mut(|memory| {
                    memory.move_focus(egui::FocusDirection::None);
                    memory.request_focus(row_focus_ids[index]);
                });
                ui.ctx().request_repaint();
            }
            if let Some(id) = selected { self.server_view.select(&id); }
            ui.separator();
            let (configuration_anchor, content_anchor, stage_anchor, acquisition_anchor, composition_anchor, records_anchor, reconciliation_anchor, preparation_anchor, activation_anchor, lifecycle_anchor, native_install_anchor, workshop_anchor, reforger_addon_anchor, maintenance_anchor) = ui.add_enabled_ui(!busy, |ui| {
                self.render_server_editor(ui);
                self.render_server_installation(ui);
                let native_install = self.render_server_native_install(ui);
                let configuration = self.render_server_configuration(ui);
                self.render_server_import_export(ui);
                self.render_server_missions(ui);
                let workshop = self.render_server_workshop(ui);
                let content = self.render_server_content(ui);
                let stage = self.render_server_content_stage(ui);
                let acquisition=self.render_server_content_acquisition(ui);
                let reforger_addons = self.render_server_reforger_addons(ui);
                self.render_server_keys(ui);
                let composition = self.render_server_deployment_composition(ui);
                let records = self.render_server_deployment_records(ui);
                let reconciliation = self.render_server_reconciliation(ui);
                let preparation = self.render_server_deployment_preparation(ui);
                let activation = self.render_server_deployment_activation(ui);
                let lifecycle = self.render_server_lifecycle(ui);
                let maintenance = self.render_server_maintenance(ui);
                self.render_consumer_recovery(ui);
                (configuration, content, stage, acquisition, composition, records, reconciliation, preparation, activation, lifecycle, native_install, workshop, reforger_addons, maintenance)
            }).inner;
            if !self.server_view.busy() && self.server_view.dialog.is_none() && !self.server_view.reforger_addon_confirmation && let Some(target) = self.server_view.pending_focus && target != consumer_recovery::CONFIRM_CANCEL_FOCUS {
                let response = if target == "server-configuration-controls" { configuration_anchor }
                    else if target == "server-content-controls" { content_anchor }
                    else if target == "server-content-stage-controls" { stage_anchor }
                    else if target == "server-content-acquisition-controls" { acquisition_anchor }
                    else if target == "server-reforger-addon-controls" { reforger_addon_anchor }
                    else if target == "server-deployment-composition-controls" { composition_anchor }
                    else if target == "server-deployment-record-controls" { records_anchor }
                    else if target == "server-reconciliation-controls" { reconciliation_anchor }
                    else if target == "server-deployment-preparation-controls" { preparation_anchor }
                    else if target == "server-deployment-activation-controls" { activation_anchor }
                    else if target == "server-lifecycle-controls" { lifecycle_anchor }
                    else if target == "server-maintenance-controls" { maintenance_anchor }
                    else if target == "server-native-install-controls" { native_install_anchor }
                    else if target == "server-workshop-controls" { workshop_anchor }
                    else if target == "server-key-controls" { self.server_view.key_controls_focus.and_then(|id| ui.ctx().read_response(id)) }
                    else { self.server_draft_focus_id(if self.server_view.editor.is_some() { target } else { "server-new-name" }).and_then(|id| ui.ctx().read_response(id)) };
                if let Some(response) = response {
                    if ui.clip_rect().intersects(response.rect) {
                        response.request_focus();
                        self.server_view.pending_focus = None;
                    } else { response.scroll_to_me(Some(egui::Align::Center)); ui.ctx().request_repaint(); }
                } else { self.server_view.pending_focus = None; }
            }
            if page_delta != 0.0 { ui.scroll_with_delta(egui::vec2(0.0, page_delta)); }
            });
        });
    }

    pub(crate) fn open_server_draft_dialog(&mut self, dialog: ServerDialog) {
        if self.server_view.busy() || self.server_view.dirty() {
            return;
        }
        self.server_view.dialog_name = self
            .server_view
            .editor
            .as_ref()
            .map(|editor| editor.draft.display_name.clone())
            .unwrap_or_default();
        self.server_view.dialog = Some(dialog);
    }
}
