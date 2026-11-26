use super::chrome::Tone;
use crate::core::game::spaces;
use crate::core::server::{DraftChange, ServerDefinition};
use crate::ui::app::Foxy;
use crate::ui::app::server::ServerPage;
use crate::ui::i18n::{is_rtl, tr, tr_fmt};
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
            self.render_server_header(ui);
            ui.add_space(8.0);
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
        if server_tabs(self, ui, &mut page, tabs, true) {
            // A primary tab opens its first step, so Deployment starts with content, not activation.
            page = page.subpages().first().copied().unwrap_or(page);
        }
        let subpages = if self.server_view.selected.is_some() {
            page.subpages()
        } else {
            &[]
        };
        if !subpages.is_empty() {
            ui.add_space(2.0);
            server_tabs(self, ui, &mut page, subpages, false);
        }
        if page != self.server_view.page {
            self.server_view.status = None;
        }
        self.server_view.page = page;
        ui.add_space(4.0);
        ui.separator();
        ui.add_space(4.0);
    }

    fn render_server_header(&mut self, ui: &mut Ui) {
        let card = self.server_card_frame();
        card.show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            let Some(editor) = &self.server_view.editor else {
                ui.heading(tr("Server instances"));
                let count = self
                    .server_view
                    .catalog
                    .as_ref()
                    .map_or(0, |catalog| catalog.instances.len());
                self.server_note(
                    ui,
                    self.i18n.tr_plural_fmt(
                        "{count} servers in {space}",
                        count as u64,
                        &[("space", spaces::active_game_space().display_name)],
                    ),
                );
                return;
            };
            let name = editor.baseline.display_name.clone();
            let id = editor.baseline.id.clone();
            let summary = server_summary_line(&editor.baseline.definition);
            let dirty = self.server_view.dirty();
            let (label, tone) = self.server_runtime_badge(&id);
            ui.horizontal_wrapped(|ui| {
                ui.heading(name);
                ui.add_space(8.0);
                self.server_status_pill(ui, &label, tone).on_hover_text(tr(
                    "Foxy reports only servers it started. Processes started outside Foxy are not tracked.",
                ));
                if dirty {
                    self.server_status_pill(ui, &tr("Unsaved changes"), Tone::Warning);
                }
            });
            self.server_note(ui, summary);
        });
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
            ui.add_space(4.0);
        }
        if let Some(error) = &self.server_view.error {
            let message = self.t(&error.to_string());
            let response = self.server_callout(ui, Tone::Danger, &message, |_| {});
            server_live_status(ui, &response, &message);
        }
        if let Some(status) = self.server_view.status {
            let message = tr(status);
            let mut dismiss = false;
            let response = self.server_callout(ui, Tone::Info, &message, |ui| {
                dismiss = ui.small_button(tr("Dismiss")).clicked();
            });
            server_live_status(ui, &response, &message);
            if dismiss {
                self.server_view.status = None;
            }
        }
        self.render_server_content_stage_status(ui);
        if self.server_view.migration_pending {
            self.server_callout(ui, Tone::Info, &tr("Legacy drafts are shown in the current schema. The next catalog save preserves a v1 backup and upgrades the catalog."), |_| {});
        }
        if self.server_view.dirty() {
            let mut open_setup = false;
            let on_setup = self.server_view.page == ServerPage::Setup;
            self.server_callout(
                ui,
                Tone::Warning,
                &tr("Save or discard draft changes before selecting another instance, reloading, or switching game spaces."),
                |ui| {
                    open_setup = !on_setup && ui.button(tr("Setup")).clicked();
                },
            );
            if open_setup {
                self.server_view.page = ServerPage::Setup;
            }
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
                if self.server_view.editor.is_some() {
                    self.server_section(ui, |app, ui| app.render_server_next_steps(ui));
                    let lifecycle = if ui.available_width() >= 900.0 {
                        ui.columns(2, |columns| {
                            let (left, right) = columns.split_at_mut(1);
                            self.server_section(&mut left[0], |app, ui| {
                                app.render_server_summary(ui)
                            });
                            self.server_section(&mut right[0], |app, ui| {
                                app.render_server_lifecycle(ui)
                            })
                        })
                    } else {
                        self.server_section(ui, |app, ui| app.render_server_summary(ui));
                        self.server_section(ui, |app, ui| app.render_server_lifecycle(ui))
                    };
                    remember("server-lifecycle-controls", lifecycle);
                } else if self.server_view.loaded {
                    self.server_section(ui, |app, ui| app.render_server_empty_state(ui));
                }
            }
            Add => self.render_add_server(ui),
            Setup => self.render_server_editor(ui),
            Installation => {
                self.server_section(ui, |app, ui| app.render_server_installation(ui));
                remember(
                    "server-native-install-controls",
                    self.server_section(ui, |app, ui| app.render_server_native_install(ui)),
                );
                self.server_section(ui, |app, ui| {
                    ui.horizontal_wrapped(|ui| {
                        ui.weak(tr(
                            "Steam sign-in sessions used for installs and Workshop downloads are managed in Tools.",
                        ));
                        if ui.button(tr("Native Steam sessions")).clicked() {
                            app.server_view.page = Tools;
                        }
                    });
                });
            }
            Import => {
                self.server_section(ui, |app, ui| app.render_server_import_export(ui));
                self.server_section(ui, |app, ui| app.render_server_instance_pack_import(ui));
            }
            Configuration => {
                remember(
                    "server-configuration-controls",
                    self.server_section(ui, |app, ui| app.render_server_configuration(ui)),
                );
                remember(
                    "server-session-controls",
                    self.server_section(ui, |app, ui| app.render_server_sessions(ui)),
                );
            }
            Addons => {
                remember(
                    "server-content-controls",
                    self.server_section(ui, |app, ui| app.render_server_content(ui)),
                );
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
                    self.server_section(ui, |app, ui| app.render_server_reforger_addons(ui)),
                );
            }
            Profiles => self.server_section(ui, |app, ui| app.render_server_addon_profiles(ui)),
            Content => {
                self.server_section(ui, |app, ui| app.render_server_missions(ui));
                remember(
                    "server-workshop-controls",
                    self.server_section(ui, |app, ui| app.render_server_workshop(ui)),
                );
                remember(
                    "server-content-stage-controls",
                    self.server_section(ui, |app, ui| app.render_server_content_stage(ui)),
                );
                remember(
                    "server-content-acquisition-controls",
                    self.server_section(ui, |app, ui| app.render_server_content_acquisition(ui)),
                );
                self.server_section(ui, |app, ui| app.render_server_keys(ui));
            }
            Records => {
                remember(
                    "server-deployment-composition-controls",
                    self.server_section(ui, |app, ui| app.render_server_deployment_composition(ui)),
                );
                remember(
                    "server-deployment-record-controls",
                    self.server_section(ui, |app, ui| app.render_server_deployment_records(ui)),
                );
                remember(
                    "server-player-publication-controls",
                    self.server_section(ui, |app, ui| app.render_server_player_publication(ui)),
                );
                remember(
                    "server-reconciliation-controls",
                    self.server_section(ui, |app, ui| app.render_server_reconciliation(ui)),
                );
            }
            Deployment => {
                remember(
                    "server-deployment-preparation-controls",
                    self.server_section(ui, |app, ui| app.render_server_deployment_preparation(ui)),
                );
                remember(
                    "server-deployment-activation-controls",
                    self.server_section(ui, |app, ui| app.render_server_deployment_activation(ui)),
                );
            }
            Operations => {
                remember(
                    "server-lifecycle-controls",
                    self.server_section(ui, |app, ui| app.render_server_lifecycle(ui)),
                );
                self.server_section(ui, |app, ui| app.render_server_owner_tools(ui));
                self.server_section(ui, |app, ui| app.render_server_monitoring(ui));
            }
            Administration => {
                remember(
                    "server-rcon-controls",
                    self.server_section(ui, |app, ui| app.render_server_rcon(ui)),
                );
                self.server_section(ui, |app, ui| app.render_arma_administration(ui));
                self.server_section(ui, |app, ui| app.render_reforger_administration(ui));
            }
            Maintenance => remember(
                "server-maintenance-controls",
                self.server_section(ui, |app, ui| app.render_server_maintenance(ui)),
            ),
            Recovery => {
                remember(
                    "server-backup-controls",
                    self.server_section(ui, |app, ui| app.render_server_backups(ui)),
                );
                self.server_section(ui, |app, ui| app.render_consumer_recovery(ui));
            }
            Tools => {
                self.server_section(ui, |app, ui| app.render_server_space_tools(ui));
                self.server_section(ui, |app, ui| app.render_server_steam_auth(ui));
                self.server_section(ui, |app, ui| app.render_server_service_export(ui));
            }
        }
        anchor
    }

    fn render_server_summary(&mut self, ui: &mut Ui) {
        let Some(editor) = &self.server_view.editor else {
            return;
        };
        let baseline = &editor.baseline;
        let installation = if baseline.installation_id.is_some() {
            tr("Linked")
        } else {
            tr("Not linked")
        };
        let enabled = |value: bool| tr(if value { "Enabled" } else { "Disabled" });
        let rows = match &baseline.definition {
            ServerDefinition::Arma3(settings) => vec![
                (tr("Server hostname"), settings.hostname.clone()),
                (tr("Game port"), settings.game_port.to_string()),
                (tr("Maximum players"), settings.max_players.to_string()),
                (
                    tr("Missions in cycle"),
                    settings
                        .mission_cycle
                        .iter()
                        .filter(|mission| mission.enabled)
                        .count()
                        .to_string(),
                ),
                (tr("BattlEye"), enabled(settings.battleye)),
                (tr("Server installation"), installation),
            ],
            ServerDefinition::Reforger(settings) => vec![
                (tr("Server name"), settings.name.clone()),
                (tr("Bind port"), settings.bind_port.to_string()),
                (tr("Maximum players"), settings.max_players.to_string()),
                (
                    tr("Scenario"),
                    settings.scenario_id.clone().unwrap_or_else(|| tr("None")),
                ),
                (tr("Addons"), settings.addons.len().to_string()),
                (tr("Server installation"), installation),
            ],
        };
        let revision = self.t_fmt(
            "Revision: {revision}",
            &[("revision", baseline.revision.to_string())],
        );
        let id = baseline.id.as_str().to_owned();
        ui.heading(tr("Server summary"));
        ui.add_space(2.0);
        self.server_facts(ui, "summary", &rows);
        self.server_note(ui, revision).on_hover_text(id);
        ui.add_space(4.0);
        ui.horizontal_wrapped(|ui| {
            if ui.button(tr("Edit configuration")).clicked() {
                self.server_view.page = ServerPage::Setup;
            }
            if ui
                .button(tr("Configure addons"))
                .on_hover_text(tr(
                    "Choose the addon profiles and requirements for this server.",
                ))
                .clicked()
            {
                self.server_view.page = ServerPage::Addons;
            }
        });
    }

    fn render_server_next_steps(&mut self, ui: &mut Ui) {
        let Some(editor) = &self.server_view.editor else {
            return;
        };
        let has_installation = editor.baseline.installation_id.is_some();
        let recovery_required = self
            .server_view
            .activation_status
            .as_ref()
            .is_some_and(|status| status.recovery_required);
        if has_installation && !recovery_required {
            return;
        }
        ui.heading(tr("Next steps"));
        ui.add_space(2.0);
        if recovery_required {
            let mut open = false;
            self.server_callout(
                ui,
                Tone::Danger,
                &tr("A deployment activation was interrupted and needs recovery before the server can start."),
                |ui| open = ui.button(tr("Native deployment activation")).clicked(),
            );
            if open {
                self.server_view.page = ServerPage::Deployment;
            }
        }
        if !has_installation {
            let mut open = false;
            self.server_callout(
                ui,
                Tone::Warning,
                &tr("No dedicated server installation is linked. Link an existing server folder or install one from Steam."),
                |ui| open = ui.button(tr("Server installation")).clicked(),
            );
            if open {
                self.server_view.page = ServerPage::Installation;
            }
        }
    }

    fn render_server_empty_state(&mut self, ui: &mut Ui) {
        let empty = self
            .server_view
            .catalog
            .as_ref()
            .is_none_or(|catalog| catalog.instances.is_empty());
        ui.heading(tr(if empty {
            "No servers yet"
        } else {
            "Select a server"
        }));
        self.server_note(
            ui,
            tr(if empty {
                "Create a server draft to configure a dedicated server, or import an existing server instance pack."
            } else {
                "Select a server or add a new one."
            }),
        );
        ui.add_space(4.0);
        if ui
            .add_enabled(
                !self.server_view.dirty(),
                egui::Button::new(tr("Add server")),
            )
            .clicked()
        {
            self.server_view.page = ServerPage::Add;
            self.server_view.focus_add_name = true;
        }
    }

    fn render_add_server(&mut self, ui: &mut Ui) {
        self.server_section(ui, |app, ui| {
            ui.heading(tr("Add server"));
            app.server_note(
                ui,
                tr("Create an empty draft, then configure its installation, settings and addons."),
            );
            ui.add_space(4.0);
            let name_id = app
                .server_draft_focus_id("server-new-name")
                .expect("new server focus id");
            let mut create = false;
            ui.horizontal_wrapped(|ui| {
                let label = ui.label(tr("New instance name"));
                let field = ui
                    .add(
                        egui::TextEdit::singleline(&mut app.server_view.new_name)
                            .id(name_id)
                            .desired_width(260.0),
                    )
                    .labelled_by(label.id);
                if std::mem::take(&mut app.server_view.focus_add_name) {
                    field.request_focus();
                }
                let can_create =
                    !app.server_view.dirty() && !app.server_view.new_name.trim().is_empty();
                create = field.lost_focus()
                    && can_create
                    && ui.input(|input| input.key_pressed(egui::Key::Enter));
                let button = app.add_sized_primary_button(
                    ui,
                    egui::Vec2::ZERO,
                    egui::Button::new(tr("Create draft")),
                    can_create,
                );
                Self::focus_outline(ui, &button);
                create |= button.clicked();
                if ui.button(tr("Cancel")).clicked() {
                    app.server_view.page = ServerPage::Overview;
                    app.server_view.pending_focus = Some("server-add-trigger");
                }
            });
            if create {
                app.start_server_draft_task(Some(DraftChange::Add {
                    name: app.server_view.new_name.clone(),
                }));
            }
        });
        let dirty = self.server_view.dirty();
        self.server_section(ui, |app, ui| {
            ui.add_enabled_ui(!dirty, |ui| app.render_server_instance_pack_import(ui))
        });
    }

    fn render_server_space_tools(&mut self, ui: &mut Ui) {
        ui.heading(tr("Shortcuts"));
        ui.add_space(2.0);
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

/// Returns true when the user picked a tab this frame.
fn server_tabs(
    app: &Foxy,
    ui: &mut Ui,
    page: &mut ServerPage,
    tabs: &[ServerPage],
    primary: bool,
) -> bool {
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
        return true;
    }
    false
}

fn server_summary_line(definition: &ServerDefinition) -> String {
    let (port, players) = match definition {
        ServerDefinition::Arma3(settings) => (settings.game_port, settings.max_players),
        ServerDefinition::Reforger(settings) => (settings.bind_port, settings.max_players),
    };
    tr_fmt(
        "Port {port} · {players} player slots",
        &[("port", port.to_string()), ("players", players.to_string())],
    )
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

    #[test]
    fn primary_tabs_open_the_first_step_of_their_own_group() {
        for tab in page_for_test() {
            let landing = tab.subpages().first().copied().unwrap_or(*tab);
            assert_eq!(landing.group(), tab.group(), "{tab:?}");
        }
        assert_eq!(
            ServerPage::Deployment.subpages().first(),
            Some(&ServerPage::Content)
        );
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
