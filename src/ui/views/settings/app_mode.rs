use crate::core::app_mode::AppMode;
use crate::ui::app::Foxy;
use crate::ui::i18n::tr;
use eframe::egui::{self, Ui};

impl Foxy {
    pub(super) fn render_application_mode_setting(&mut self, ui: &mut Ui, changed: &mut bool) {
        let mut selected = self
            .settings_view_state
            .app_mode
            .effective(self.launch_mode_override);
        let previous = selected;
        ui.horizontal_wrapped(|ui| {
            ui.label(tr("Application mode"));
            ui.add_enabled_ui(
                self.launch_mode_override.is_none() && !self.mode_ownership.busy(),
                |ui| {
                    let client = ui.radio(selected == AppMode::Client, tr("Client mode"));
                    let server = ui.radio(selected == AppMode::Server, tr("Server mode (WIP)"));
                    let client_focused = ui.memory(|memory| memory.has_focus(client.id));
                    let server_focused = ui.memory(|memory| memory.has_focus(server.id));
                    for response in [&client, &server] {
                        if ui.memory(|memory| memory.has_focus(response.id)) {
                            ui.memory_mut(|memory| {
                                memory.set_focus_lock_filter(
                                    response.id,
                                    egui::EventFilter {
                                        horizontal_arrows: true,
                                        vertical_arrows: true,
                                        ..Default::default()
                                    },
                                );
                            });
                        }
                    }
                    let mut choose_client = client.clicked();
                    let mut choose_server = server.clicked();
                    if client_focused || server_focused {
                        ui.input_mut(|input| {
                            if input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowLeft)
                                || input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp)
                            {
                                choose_client = true;
                            }
                            if input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowRight)
                                || input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown)
                            {
                                choose_server = true;
                            }
                            if input.consume_key(egui::Modifiers::NONE, egui::Key::Enter)
                                || input.consume_key(egui::Modifiers::NONE, egui::Key::Space)
                            {
                                choose_client |= client_focused;
                                choose_server |= server_focused;
                            }
                        });
                    }
                    if choose_client {
                        selected = AppMode::Client;
                        client.request_focus();
                    }
                    if choose_server {
                        selected = AppMode::Server;
                        server.request_focus();
                    }
                },
            );
        });
        if selected != previous {
            self.settings_view_state.app_mode = selected;
            *changed = true;
        }
        if self.launch_mode_override.is_some() {
            ui.label(tr(
                "This session uses --mode. Restart without it to change the saved mode in Settings.",
            ));
        }
        self.render_owner_status(ui);
        ui.label(tr(
            "Server mode is a preview. Switching modes does not install or start a dedicated server.",
        ));
    }
}
