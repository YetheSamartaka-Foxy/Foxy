use egui::Ui;
use foxy_steam::auth::AuthPhase;

use crate::core::server::steam_auth::{SteamAuthOperation, SteamAuthStatus};
use crate::ui::app::Foxy;
use crate::ui::i18n::{tr, tr_fmt};

impl Foxy {
    pub(crate) fn render_server_steam_auth(&mut self, ui: &mut Ui) {
        if !crate::core::game::registry()
            .active()
            .server_capabilities()
            .steam_authentication
        {
            return;
        }
        ui.collapsing(tr("Native Steam sessions"), |ui| {
            ui.label(tr("Prepare a protected native Steam session for later provider operations. Login does not install apps or Workshop content."));
            ui.label(tr("Refresh tokens are retained privately for reuse. Passwords and Guard codes are not saved. Steam may require another login when a token expires or is revoked."));
            let busy=self.server_view.busy();
            let error_color=self.color_text_error();
            let mut operation=None;
            let state=&mut self.server_view.steam_auth;
            ui.add_enabled_ui(!busy, |ui| {
                ui.horizontal_wrapped(|ui| {
                    let label=ui.label(tr("Local Steam session alias"));
                    if ui.add(egui::TextEdit::singleline(&mut state.alias)).labelled_by(label.id).changed() {
                        state.outcome=None;
                        state.forget_confirmed=false;
                    }
                });
                let account=ui.horizontal_wrapped(|ui| {
                    let label=ui.label(tr("Steam account name"));
                    let account=ui.add(egui::TextEdit::singleline(state.account_name.expose_mut()).desired_width(240.0)).labelled_by(label.id);
                    let label=ui.label(tr("Steam password"));
                    ui.add(egui::TextEdit::singleline(state.password.expose_mut()).password(true).desired_width(240.0)).labelled_by(label.id);
                    account.id
                }).inner;
                ui.horizontal_wrapped(|ui| {
                    if ui.add_enabled(!state.account_name.expose().is_empty() && !state.password.expose().is_empty(),egui::Button::new(tr("Prepare native Steam session"))).clicked() {
                        operation=Some((SteamAuthOperation::Password {
                            account_name:std::mem::take(&mut state.account_name),
                            password:std::mem::take(&mut state.password),
                        },account));
                    }
                    let inspect=ui.button(tr("Inspect prepared Steam session"));
                    if inspect.clicked() {operation=Some((SteamAuthOperation::Inspect,inspect.id));}
                    let verify=ui.button(tr("Verify prepared Steam session"));
                    if verify.clicked() {operation=Some((SteamAuthOperation::Resume,verify.id));}
                    let anonymous=ui.button(tr("Test anonymous Steam connection"));
                    if anonymous.clicked() {operation=Some((SteamAuthOperation::Anonymous,anonymous.id));}
                });
                let confirm=ui.checkbox(&mut state.forget_confirmed,tr("Confirm forgetting this local Steam session"));
                if ui.add_enabled(state.forget_confirmed,egui::Button::new(tr("Forget local Steam session"))).clicked() {operation=Some((SteamAuthOperation::Forget,confirm.id));}
            });
            if let Some(job)=&state.job {
                let phase=job.control.phase();
                ui.label(tr(steam_phase_text(phase)));
                let wants_code=matches!(phase,AuthPhase::EmailCode|AuthPhase::DeviceCode|AuthPhase::CodeRejectedEmail|AuthPhase::CodeRejectedDevice);
                if wants_code && !job.control.is_cancelled() {
                    ui.horizontal_wrapped(|ui| {
                        let label=ui.label(tr("Steam Guard code"));
                        let field=ui.add(egui::TextEdit::singleline(state.code.expose_mut()).password(true).desired_width(160.0)).labelled_by(label.id);
                        if state.code_focus_phase!=Some(phase) {
                            field.request_focus();
                            state.code_focus_phase=Some(phase);
                        }
                        let submit=ui.button(tr("Submit Steam Guard code"));
                        if submit.clicked() || (field.lost_focus() && ui.input(|input|input.key_pressed(egui::Key::Enter))) {
                            let code=std::mem::take(&mut state.code);
                            if let Err(error)=job.control.submit_code(code) {state.error=Some(error);}
                        }
                    });
                }
                let cancel=ui.button(tr("Cancel Steam authentication"));
                if !state.cancel_focused && !wants_code {
                    cancel.request_focus();
                    state.cancel_focused=true;
                }
                if cancel.clicked() {job.control.cancel();}
                if job.control.is_cancelled() {ui.label(tr("Waiting for the native Steam connection to close..."));}
            } else if !busy && let Some(id)=state.restore_focus.take() {
                ui.memory_mut(|memory|memory.request_focus(id));
            }
            if let Some(error)=state.error {ui.colored_label(error_color,tr(&error.to_string()));}
            if let Some(outcome)=&state.outcome {
                if let Some(error)=outcome.error {ui.colored_label(error_color,tr(&error.to_string()));}
                if outcome.status==SteamAuthStatus::Forgotten {
                    ui.label(tr("Local Steam session forgotten; no record remains for this alias."));
                } else if outcome.status!=SteamAuthStatus::Inspected {
                    if outcome.authenticated || outcome.disconnected {
                        ui.label(tr_fmt("Steam authentication verified: {verified}; connection closed: {closed}",&[("verified",outcome.authenticated.to_string()),("closed",outcome.disconnected.to_string())]));
                    } else {
                        ui.label(tr("No native Steam connection was opened."));
                    }
                }
                let presence=match outcome.prepared {Some(true)=>"Present",Some(false)=>"Missing",None=>"Unknown"};
                ui.label(tr_fmt("Prepared Steam session: {state}",&[("state",tr(presence))]));
                ui.label(tr("Stored-session presence does not prove current Steam authentication or app entitlement."));
            }
            if let Some((operation,focus))=operation {
                self.server_view.steam_auth.restore_focus=Some(focus);
                self.start_server_steam_auth(operation);
            }
        });
    }
}

fn steam_phase_text(phase: AuthPhase) -> &'static str {
    match phase {
        AuthPhase::Connecting => "Connecting to native Steam authentication...",
        AuthPhase::Authenticating => "Verifying the native Steam session...",
        AuthPhase::EmailCode => "Enter the Steam Guard code from email.",
        AuthPhase::DeviceCode => "Enter the Steam Guard authenticator code.",
        AuthPhase::MobileApproval => "Approve this login in the Steam mobile app.",
        AuthPhase::CodeRejectedEmail => "Email code rejected or expired. Enter another code.",
        AuthPhase::CodeRejectedDevice => {
            "Authenticator code rejected or expired. Enter another code."
        }
        AuthPhase::Authenticated => "Native Steam authentication verified.",
    }
}
