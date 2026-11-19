use crate::core::game::spaces;
use crate::core::server::DraftChange;
use crate::ui::app::Foxy;
use crate::ui::app::server::ServerPage;
use crate::ui::i18n::{is_rtl, tr};
use eframe::egui::{self, Response, Ui};

impl Foxy {
    pub(crate) fn render_server_overview(&mut self, ui: &mut Ui) {
        let supported = self.server_drafts_supported();
        if !self.mode_ownership.ready() || !supported {
            egui::ScrollArea::vertical()
                .id_salt("server-workspace-unavailable")
                .show(ui, |ui| {
                    ui.heading(tr("Server instances"));
                    ui.label(self.t_fmt(
                        "Game space: {name}",
                        &[("name", spaces::active_game_space().display_name)],
                    ));
                    self.render_owner_status(ui);
                    if !supported {
                        ui.label(tr(
                            "Dedicated server management is unavailable for this game.",
                        ));
                    }
                    self.render_server_space_tools(ui);
                });
            self.render_server_session_confirmation(ui.ctx());
            return;
        }
        if !self.server_view.loaded && !self.server_view.busy() {
            self.start_server_draft_task(None);
        }
        self.render_server_workspace(ui);
        self.render_server_session_confirmation(ui.ctx());
    }

    pub(in crate::ui::views) fn render_server_workspace(&mut self, ui: &mut Ui) {
        let compact = ui.available_width() < 840.0;
        let mut frame = egui::Frame::side_top_panel(&ui.ctx().global_style());
        frame.fill = self.color_main_bg();
        frame.inner_margin = egui::Margin::same(10);
        let space_id = spaces::active_game_space().space_id;
        let sidebar_id = ("server-sidebar", &space_id);
        let sidebar = if compact {
            egui::Panel::top(sidebar_id)
        } else if is_rtl() {
            egui::Panel::right(sidebar_id).exact_size(250.0)
        } else {
            egui::Panel::left(sidebar_id).exact_size(250.0)
        };
        sidebar.resizable(false).frame(frame).show(ui, |ui| {
            self.render_server_sidebar(ui, compact);
        });
        if self.pending_game_space_switch.is_none() {
            self.poll_server_workspace_runtime(ui);
        }
        egui::Frame::NONE.inner_margin(12.0).show(ui, |ui| {
            egui::Frame::group(ui.style())
                .fill(self.color_card_bg())
                .inner_margin(egui::Margin::symmetric(12, 9))
                .show(ui, |ui| {
                    ui.set_min_width(ui.available_width());
                    if let Some(editor) = &self.server_view.editor {
                        ui.heading(&editor.baseline.display_name);
                        let status = self
                            .observed_server_status(&editor.baseline.id)
                            .map(|status| super::lifecycle::runtime_phase_label(status.phase))
                            .unwrap_or_else(|| tr("Unknown"));
                        ui.label(status);
                    } else {
                        ui.heading(tr("Server instances"));
                    }
                });
            ui.add_space(6.0);
            self.render_server_tabs(ui);
            let page = self.server_view.page;

            let page_delta = if self.server_view.dialog.is_none()
                && !self.server_view.reforger_addon_confirmation
                && !self.server_view.sessions.confirmation
                && !self.server_view.consumer_recovery_confirmation
            {
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
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .id_salt((
                    "server-page",
                    &space_id,
                    page,
                    self.server_view.selected.as_ref().map(|id| id.as_str()),
                ))
                .show(ui, |ui| {
                    self.render_server_workspace_feedback(ui);
                    let busy = self.server_view.controls_busy()
                        || self.pending_game_space_switch.is_some();
                    let anchor = ui
                        .add_enabled_ui(!busy, |ui| self.render_server_workspace_page(ui, page))
                        .inner;
                    self.restore_server_workspace_focus(ui, anchor);
                    if page_delta != 0.0 {
                        ui.scroll_with_delta(egui::vec2(0.0, page_delta));
                    }
                });
        });
    }

    fn render_server_tabs(&mut self, ui: &mut Ui) {
        use ServerPage::*;
        let mut page = self.server_view.page;
        let tabs: &[ServerPage] = if self.server_view.selected.is_some() {
            &[
                Overview, Setup, Addons, Deployment, Operations, Recovery, Tools,
            ]
        } else if crate::core::game::registry()
            .active()
            .server_capabilities()
            .mission_library
            || crate::core::game::registry()
                .active()
                .server_capabilities()
                .workshop_acquisition
        {
            &[Overview, Content, Tools]
        } else {
            &[Overview, Tools]
        };
        server_tabs(self, ui, &mut page, tabs, true);
        let subpages = if self.server_view.selected.is_some() {
            page.subpages()
        } else {
            &[]
        };
        if !subpages.is_empty() {
            server_tabs(self, ui, &mut page, subpages, false);
        }
        self.server_view.page = page;
        ui.separator();
    }

    fn render_server_workspace_feedback(&mut self, ui: &mut Ui) {
        if self
            .server_view
            .task
            .as_ref()
            .is_some_and(|task| !task.passive && task.kind.is_none())
        {
            ui.horizontal_wrapped(|ui| {
                ui.add(crate::ui::app::PacedSpinner::new());
                ui.label(tr("Reading or saving server drafts..."));
            });
        }
        if let Some(error) = &self.server_view.error {
            let message = self.t(&error.to_string());
            let response = ui.colored_label(self.color_text_error(), &message);
            server_live_status(ui, &response, &message);
        }
        if let Some(status) = self.server_view.status {
            let message = tr(status);
            let response = ui.label(&message);
            server_live_status(ui, &response, &message);
        }
        self.render_server_content_stage_status(ui);
        if self.server_view.migration_pending {
            ui.label(tr("Legacy drafts are shown in the current schema. The next catalog save preserves a v1 backup and upgrades the catalog."));
        }
        if self.server_view.dirty() {
            ui.horizontal_wrapped(|ui| {
                ui.label(tr("Save or discard draft changes before selecting another instance, reloading, or switching game spaces."));
                if self.server_view.page != ServerPage::Setup && ui.button(tr("Setup")).clicked() {
                    self.server_view.page = ServerPage::Setup;
                }
            });
        }
    }

    fn render_server_workspace_page(
        &mut self,
        ui: &mut Ui,
        page: ServerPage,
    ) -> Option<(&'static str, Response)> {
        use ServerPage::*;
        let target = self.server_view.pending_focus;
        let mut anchor = None;
        let mut remember = |key, response: Option<Response>| {
            if target == Some(key) {
                anchor = response.map(|response| (key, response));
            }
        };
        if self.server_view.selected.is_none() && !matches!(page, Overview | Tools | Add | Content)
        {
            self.server_view.page = Overview;
            return None;
        }
        match page {
            Overview => {
                if let Some(editor) = &self.server_view.editor {
                    let has_installation = editor.baseline.installation_id.is_some();
                    ui.label(self.t_fmt(
                        "Revision: {revision}",
                        &[("revision", editor.baseline.revision.to_string())],
                    ));
                    if !has_installation {
                        ui.horizontal_wrapped(|ui| {
                            ui.label(tr("No installation"));
                            if ui.button(tr("Server installation")).clicked() {
                                self.server_view.page = Installation;
                            }
                        });
                    }
                    if self
                        .server_view
                        .activation_status
                        .as_ref()
                        .is_some_and(|status| status.recovery_required)
                        && ui.button(tr("Native deployment activation")).clicked()
                    {
                        self.server_view.page = Deployment;
                    }
                    remember(
                        "server-lifecycle-controls",
                        self.render_server_lifecycle(ui),
                    );
                } else if self.server_view.loaded {
                    ui.label(tr("Select a server or add a new one."));
                }
            }
            Add => self.render_add_server(ui),
            Setup => {
                self.render_server_addons_link(ui);
                self.render_server_editor(ui);
            }
            Installation => {
                self.render_server_installation(ui);
                remember(
                    "server-native-install-controls",
                    self.render_server_native_install(ui),
                );
                if ui.button(tr("Native Steam sessions")).clicked() {
                    self.server_view.page = Tools;
                }
            }
            Import => {
                self.render_server_import_export(ui);
                self.render_server_instance_pack_import(ui);
            }
            Configuration => {
                remember(
                    "server-configuration-controls",
                    self.render_server_configuration(ui),
                );
                remember("server-session-controls", self.render_server_sessions(ui));
            }
            Addons => {
                remember("server-content-controls", self.render_server_content(ui));
                if crate::core::game::registry()
                    .active()
                    .server_capabilities()
                    .definition_kind
                    == Some(crate::core::server::ServerDefinitionKind::Reforger)
                    && ui
                        .button(tr("Edit scenario and addon intent"))
                        .on_hover_text(tr(
                            "Set the scenario and requested addon versions in Setup.",
                        ))
                        .clicked()
                {
                    self.server_view.page = Setup;
                }
                remember(
                    "server-reforger-addon-controls",
                    self.render_server_reforger_addons(ui),
                );
            }
            Profiles => self.render_server_addon_profiles(ui),
            Content => {
                self.render_server_addons_link(ui);
                self.render_server_missions(ui);
                remember("server-workshop-controls", self.render_server_workshop(ui));
                remember(
                    "server-content-stage-controls",
                    self.render_server_content_stage(ui),
                );
                remember(
                    "server-content-acquisition-controls",
                    self.render_server_content_acquisition(ui),
                );
                self.render_server_keys(ui);
            }
            Records => {
                remember(
                    "server-deployment-composition-controls",
                    self.render_server_deployment_composition(ui),
                );
                remember(
                    "server-deployment-record-controls",
                    self.render_server_deployment_records(ui),
                );
                remember(
                    "server-player-publication-controls",
                    self.render_server_player_publication(ui),
                );
                remember(
                    "server-reconciliation-controls",
                    self.render_server_reconciliation(ui),
                );
            }
            Deployment => {
                self.render_server_addons_link(ui);
                remember(
                    "server-deployment-preparation-controls",
                    self.render_server_deployment_preparation(ui),
                );
                remember(
                    "server-deployment-activation-controls",
                    self.render_server_deployment_activation(ui),
                );
            }
            Operations => {
                self.render_server_owner_tools(ui);
                remember(
                    "server-lifecycle-controls",
                    self.render_server_runtime_refresh(ui),
                );
                self.render_server_monitoring(ui);
            }
            Administration => {
                remember("server-rcon-controls", self.render_server_rcon(ui));
                self.render_arma_administration(ui);
                self.render_reforger_administration(ui);
            }
            Maintenance => remember(
                "server-maintenance-controls",
                self.render_server_maintenance(ui),
            ),
            Recovery => {
                remember("server-backup-controls", self.render_server_backups(ui));
                self.render_consumer_recovery(ui);
            }
            Tools => {
                self.render_server_space_tools(ui);
                self.render_server_steam_auth(ui);
                self.render_server_service_export(ui);
            }
        }
        anchor
    }

    fn render_server_addons_link(&mut self, ui: &mut Ui) {
        if self.server_view.selected.is_some()
            && ui
                .button(tr("Configure addons"))
                .on_hover_text(tr(
                    "Choose the addon profiles and requirements for this server.",
                ))
                .clicked()
        {
            self.server_view.page = ServerPage::Addons;
        }
    }

    fn render_add_server(&mut self, ui: &mut Ui) {
        ui.heading(tr("Add server"));
        let name_id = self
            .server_draft_focus_id("server-new-name")
            .expect("new server focus id");
        ui.horizontal_wrapped(|ui| {
            let label = ui.label(tr("New instance name"));
            let field = ui
                .add(
                    egui::TextEdit::singleline(&mut self.server_view.new_name)
                        .id(name_id)
                        .desired_width(210.0),
                )
                .labelled_by(label.id);
            if std::mem::take(&mut self.server_view.focus_add_name) {
                field.request_focus();
            }
            if ui
                .add_enabled(
                    !self.server_view.dirty() && !self.server_view.new_name.trim().is_empty(),
                    egui::Button::new(tr("Create draft")),
                )
                .clicked()
            {
                self.start_server_draft_task(Some(DraftChange::Add {
                    name: self.server_view.new_name.clone(),
                }));
            }
            if ui.button(tr("Cancel")).clicked() {
                self.server_view.page = ServerPage::Overview;
                self.server_view.pending_focus = Some("server-add-trigger");
            }
        });
        ui.separator();
        ui.add_enabled_ui(!self.server_view.dirty(), |ui| {
            self.render_server_instance_pack_import(ui)
        });
    }

    fn render_server_space_tools(&mut self, ui: &mut Ui) {
        ui.horizontal_wrapped(|ui| {
            if ui.button(tr("Game spaces")).clicked() {
                self.open_game_spaces_view();
            }
            if ui.button(tr("Application settings")).clicked() {
                self.open_settings_view();
                self.settings_view_state.current_tab = "Application".into();
            }
            if self.update_modal_open && ui.button(tr("Client update progress")).clicked() {
                self.server_view.show_client_progress = true;
            }
        });
    }

    fn restore_server_workspace_focus(
        &mut self,
        ui: &Ui,
        anchor: Option<(&'static str, Response)>,
    ) {
        if self.server_view.busy()
            || self.server_view.dialog.is_some()
            || self.server_view.reforger_addon_confirmation
            || self.server_view.sessions.confirmation
            || self.server_view.consumer_recovery_confirmation
        {
            return;
        }
        let Some(target) = self.server_view.pending_focus else {
            return;
        };
        if target == "server-add-trigger" {
            return;
        }
        let response = anchor
            .filter(|(key, _)| *key == target)
            .map(|(_, response)| response)
            .or_else(|| {
                let id = if target == "server-key-controls" {
                    self.server_view.key_controls_focus
                } else {
                    self.server_draft_focus_id(target)
                }?;
                ui.ctx().read_response(id)
            });
        if let Some(response) = response.filter(|response| response.enabled()) {
            if !ui.clip_rect().intersects(response.rect) {
                response.scroll_to_me(Some(egui::Align::Center));
                ui.ctx().request_repaint();
                return;
            }
            response.request_focus();
        }
        self.server_view.pending_focus = None;
    }
}

fn server_tabs(app: &Foxy, ui: &mut Ui, page: &mut ServerPage, tabs: &[ServerPage], primary: bool) {
    use ServerPage::*;
    let selected = if primary && tabs.contains(&page.group()) {
        page.group()
    } else {
        *page
    };
    let selected_index = tabs
        .iter()
        .position(|tab| *tab == selected)
        .unwrap_or(usize::MAX);
    let descriptions = tabs
        .iter()
        .map(|tab| {
            let (label, description) = match tab {
                Overview => (
                    "Overview",
                    "See runtime status and start, stop or restart this server.",
                ),
                Add => (
                    "Add server",
                    "Create a draft or import a server instance pack.",
                ),
                Setup if !primary => (
                    "Configuration",
                    "Edit this server's identity, network and game settings.",
                ),
                Setup => (
                    "Setup",
                    "Configure this server, its installation and its game settings.",
                ),
                Installation => (
                    "Server installation",
                    "Inspect, bind or manage a dedicated server installation.",
                ),
                Import => (
                    "Import",
                    "Review native configuration files and portable instance packs.",
                ),
                Configuration => (
                    "Saved configuration revisions",
                    "Preview and review saved server configuration and session settings.",
                ),
                Addons if !primary => (
                    "Server loadout",
                    "Select profiles for this server and review the effective addon selection.",
                ),
                Addons => (
                    "Addons",
                    "Choose the addon profiles and requirements for this server.",
                ),
                Profiles => (
                    "Addon profiles",
                    "Create named addon sets and manage their source folders.",
                ),
                Deployment if !primary => (
                    "Native deployment activation",
                    "Prepare and activate the reviewed runtime for this server.",
                ),
                Deployment => (
                    "Deployment",
                    "Acquire, verify and prepare the content used by a server runtime.",
                ),
                Content => (
                    "Server content",
                    "Manage staged content, missions, Workshop items and keys.",
                ),
                Records => (
                    "Deployment records",
                    "Inspect, verify and select retained deployment records.",
                ),
                Operations if !primary => (
                    "Server monitoring",
                    "Inspect the running server, resource usage and logs.",
                ),
                Operations => (
                    "Operations",
                    "Monitor this server and manage its jobs and administration.",
                ),
                Administration => (
                    "Administration",
                    "Review supported live administration and remote console actions.",
                ),
                Maintenance => (
                    "Scheduled maintenance",
                    "Review scheduled jobs and their latest outcomes.",
                ),
                Recovery => (
                    "Backups and recovery",
                    "Review backup, restore and crash recovery options.",
                ),
                Tools => (
                    "Tools",
                    "Manage the current game space's Steam sessions and unattended operation.",
                ),
            };
            (label, tab.label(), tr(description))
        })
        .collect::<Vec<_>>();
    let labels = descriptions
        .iter()
        .map(|(label, id, description)| (*label, *id, description.as_str()))
        .collect::<Vec<_>>();
    if let Some(index) = app.render_adaptive_tab_bar_described(
        ui,
        &labels,
        selected_index,
        ("server-navigation", primary),
        !primary,
    ) {
        *page = tabs[index];
    }
}

pub(super) fn server_live_status(ui: &Ui, response: &Response, message: &str) {
    use egui::accesskit::{Live, Role};
    ui.ctx().accesskit_node_builder(response.id, |node| {
        node.set_role(Role::Status);
        node.set_label(message);
        node.set_live(Live::Polite);
        node.set_live_atomic();
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_tabs_leave_vertical_space_for_the_page() {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let app = Foxy::new_headless(&ctx);
        let mut page = ServerPage::Operations;
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(520.0, 600.0),
            )),
            ..Default::default()
        };
        let mut output = ctx.run_ui(input, |ui| {
            egui::CentralPanel::default().show(ui, |ui| {
                let before = ui.next_widget_position().y;
                server_tabs(&app, ui, &mut page, page_for_test(), true);
                let used = ui.next_widget_position().y - before;
                assert!((55.0..100.0).contains(&used), "Tabs consumed {used} points");
            });
        });
        output.textures_delta.clear();
        assert_eq!(page, ServerPage::Operations);
        let tree = output.platform_output.accesskit_update.unwrap();
        let tabs = tree
            .nodes
            .iter()
            .filter(|(_, node)| node.role() == egui::accesskit::Role::Tab)
            .collect::<Vec<_>>();
        assert_eq!(tabs.len(), page_for_test().len());
        assert!(tabs.iter().all(|(_, node)| node.description().is_some()));
    }

    fn page_for_test() -> &'static [ServerPage] {
        &[
            ServerPage::Overview,
            ServerPage::Setup,
            ServerPage::Addons,
            ServerPage::Deployment,
            ServerPage::Operations,
            ServerPage::Recovery,
            ServerPage::Tools,
        ]
    }
}
