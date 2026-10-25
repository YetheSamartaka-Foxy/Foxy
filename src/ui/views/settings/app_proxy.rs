use crate::core::utils::proxy::validate_proxy_url;
use crate::ui::app::Foxy;
use crate::ui::i18n::tr;
use crate::ui::types::ProxyMode;
use eframe::egui::{self, RichText, TextEdit, Ui};

use super::render_wrapped_info_row;

/// What a proxy field did this frame.
#[derive(Default)]
struct FieldChange {
    edited: bool,
    committed: bool,
}

impl Foxy {
    /// Proxy mode and, for a manual proxy, its address, credentials, and
    /// bypass list. Text fields apply when they lose focus, so a half-typed
    /// address never reaches the clients.
    pub(super) fn render_application_settings_proxy(
        &mut self,
        ui: &mut Ui,
        horizontal_padding: f32,
        changed: &mut bool,
    ) {
        let mut apply = false;

        ui.horizontal(|ui| {
            ui.add_space(horizontal_padding);
            let row_label = ui.label(tr("Proxy"));
            let mode_label = |mode: ProxyMode| -> String {
                match mode {
                    ProxyMode::System => tr("System proxy settings"),
                    ProxyMode::None => tr("No proxy"),
                    ProxyMode::Manual => tr("Manual proxy"),
                }
            };
            let combo = egui::ComboBox::from_id_salt("settings_proxy_mode")
                .selected_text(mode_label(self.settings_view_state.proxy_mode))
                .show_ui(ui, |ui| {
                    for (mode, description) in [
                        (
                            ProxyMode::System,
                            tr("Use the proxy set in Windows, or in the HTTP_PROXY and HTTPS_PROXY environment variables."),
                        ),
                        (
                            ProxyMode::None,
                            tr("Connect directly, even when Windows has a proxy set."),
                        ),
                        (
                            ProxyMode::Manual,
                            tr("Use the HTTP or HTTPS proxy entered below."),
                        ),
                    ] {
                        let response = ui
                            .selectable_value(
                                &mut self.settings_view_state.proxy_mode,
                                mode,
                                mode_label(mode),
                            )
                            .on_hover_text(description.as_str());
                        if response.changed() {
                            apply = true;
                        }
                    }
                });
            let combo_response = combo.response.labelled_by(row_label.id).on_hover_text(tr(
                "How Foxy reaches repositories, app updates, Steam Workshop details and images.",
            ));
            if combo_response.hovered() {
                ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
            }
            ui.add_space(horizontal_padding);
        });

        if self.settings_view_state.proxy_mode == ProxyMode::Manual {
            let field_width = (ui.available_width() - 2.0 * horizontal_padding).max(120.0);
            let address = Self::render_proxy_text_field(
                ui,
                horizontal_padding,
                field_width,
                tr("Proxy address"),
                &mut self.settings_view_state.proxy_url,
                "http://proxy.example:8080",
                false,
            );
            if let Err(reason) = validate_proxy_url(&self.settings_view_state.proxy_url) {
                render_wrapped_info_row(
                    ui,
                    horizontal_padding,
                    RichText::new(tr(reason)).color(self.color_error()),
                );
            }
            let username = Self::render_proxy_text_field(
                ui,
                horizontal_padding,
                field_width,
                tr("Proxy user name (optional)"),
                &mut self.settings_view_state.proxy_username,
                "",
                false,
            );
            let password = Self::render_proxy_text_field(
                ui,
                horizontal_padding,
                field_width,
                tr("Proxy password (optional)"),
                &mut self.settings_view_state.proxy_password,
                "",
                true,
            );
            let bypass = Self::render_proxy_text_field(
                ui,
                horizontal_padding,
                field_width,
                tr("Connect directly to (comma-separated)"),
                &mut self.settings_view_state.proxy_bypass,
                "localhost, 127.0.0.1, .example.invalid",
                false,
            );
            render_wrapped_info_row(
                ui,
                horizontal_padding,
                RichText::new(tr(
                    "The password is stored in Foxy's settings file as plain text.",
                ))
                .italics()
                .color(self.color_text_dim()),
            );
            for field in [address, username, password, bypass] {
                *changed |= field.edited;
                apply |= field.committed;
            }
        }

        if apply {
            crate::core::utils::runtime_settings::apply_runtime_settings(&self.settings_view_state);
            *changed = true;
        }
    }

    fn render_proxy_text_field(
        ui: &mut Ui,
        horizontal_padding: f32,
        field_width: f32,
        label: String,
        value: &mut String,
        hint: &str,
        password: bool,
    ) -> FieldChange {
        let mut change = FieldChange::default();
        let label_id = ui
            .horizontal(|ui| {
                ui.add_space(horizontal_padding);
                ui.label(label.as_str()).id
            })
            .inner;
        ui.horizontal(|ui| {
            ui.add_space(horizontal_padding);
            let response = ui
                .add(
                    TextEdit::singleline(value)
                        .password(password)
                        .hint_text(hint)
                        .desired_width(field_width),
                )
                .labelled_by(label_id);
            change.edited = response.changed();
            change.committed = response.lost_focus();
            ui.add_space(horizontal_padding);
        });
        change
    }
}
