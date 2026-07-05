use crate::core::arma3_profiles::{
    Arma3Profile, ProfileNameError, ProfileOpError, clone_profile, delete_profile,
    is_arma3_running, is_vanilla_profiles_location, other_profiles_root, rename_profile,
    validate_new_profile_name,
};
use crate::core::utils::app_paths;
use crate::ui::app::Foxy;
use crate::ui::i18n::tr;
use eframe::egui::{self, Button, RichText, TextEdit, Ui};
use log::warn;
use std::path::PathBuf;

use super::render_wrapped_info_row;

/// Which management operation is pending confirmation in the modal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arma3ProfileActionKind {
    Rename,
    Clone,
    Delete,
}

/// A profile management action opened from the settings profile list,
/// carrying the snapshot of the profile it targets and the name input.
#[derive(Debug, Clone)]
pub struct Arma3ProfileAction {
    pub kind: Arma3ProfileActionKind,
    pub profile: Arma3Profile,
    pub name_input: String,
}

impl Foxy {
    /// List of detected Arma 3 profiles with rename/clone/delete actions,
    /// rendered in the application settings under the profiles directory.
    pub(super) fn render_arma3_profile_management(&mut self, ui: &mut Ui, horizontal_padding: f32) {
        ui.horizontal(|ui| {
            ui.add_space(horizontal_padding);
            ui.label(tr("Detected Arma 3 Profiles"));
            ui.add_space(horizontal_padding);
        });

        let profiles_dir = self.settings_view_state.arma3_profiles_directory.trim();
        if !profiles_dir.is_empty()
            && is_vanilla_profiles_location(std::path::Path::new(profiles_dir))
        {
            render_wrapped_info_row(
                ui,
                horizontal_padding,
                RichText::new(tr(
                    "The configured profiles directory is the standard Arma 3 profile location. Foxy will not pass it to the game; leave the field empty unless profiles should be stored somewhere else.",
                ))
                .italics()
                .color(self.color_text_error()),
            );
        }

        if self.detected_arma3_profiles.is_empty() {
            render_wrapped_info_row(
                ui,
                horizontal_padding,
                RichText::new(tr("No Arma 3 profiles detected."))
                    .italics()
                    .color(self.color_text_dim()),
            );
            return;
        }

        let profiles = self.detected_arma3_profiles.clone();
        let active_profile = self.detected_active_arma3_profile.clone();
        for profile in &profiles {
            ui.horizontal(|ui| {
                ui.add_space(horizontal_padding);

                let mut label = profile.name.clone();
                if profile.is_default {
                    label = format!("{} ({})", label, tr("default"));
                }
                if active_profile.as_deref() == Some(profile.name.as_str()) {
                    label = format!("{} - {}", label, tr("last used"));
                }
                ui.label(label)
                    .on_hover_text(profile.path.display().to_string());

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.add_space(horizontal_padding);

                    let delete_enabled = !profile.is_default;
                    let delete_button = ui
                        .add_enabled(delete_enabled, Button::new(tr("Delete")))
                        .on_hover_text(tr(
                            "Move this profile folder to the Foxy backup directory.",
                        ))
                        .on_disabled_hover_text(tr(
                            "The default Arma 3 profile cannot be deleted.",
                        ));
                    if delete_button.hovered() && delete_enabled {
                        ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                    }
                    if delete_button.clicked() {
                        self.pending_arma3_profile_action = Some(Arma3ProfileAction {
                            kind: Arma3ProfileActionKind::Delete,
                            profile: profile.clone(),
                            name_input: String::new(),
                        });
                    }

                    let clone_button = ui.add(Button::new(tr("Clone"))).on_hover_text(tr(
                        "Copy this profile's settings, keybinds, and editor preferences into a new profile.",
                    ));
                    if clone_button.hovered() {
                        ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                    }
                    if clone_button.clicked() {
                        self.pending_arma3_profile_action = Some(Arma3ProfileAction {
                            kind: Arma3ProfileActionKind::Clone,
                            profile: profile.clone(),
                            name_input: format!("{} 2", profile.name),
                        });
                    }

                    let rename_button = ui
                        .add(Button::new(tr("Rename")))
                        .on_hover_text(tr("Rename this profile's folder and profile files."));
                    if rename_button.hovered() {
                        ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                    }
                    if rename_button.clicked() {
                        self.pending_arma3_profile_action = Some(Arma3ProfileAction {
                            kind: Arma3ProfileActionKind::Rename,
                            profile: profile.clone(),
                            name_input: profile.name.clone(),
                        });
                    }
                });
            });
        }

        render_wrapped_info_row(
            ui,
            horizontal_padding,
            RichText::new(tr(
                "Profile changes are blocked while Arma 3 is running. Deleted profiles are moved to the Foxy backup directory instead of being removed permanently.",
            ))
            .italics()
            .color(self.color_text_dim()),
        );
    }

    /// Confirmation modal for the pending profile action.
    pub(super) fn render_arma3_profile_action_modal(&mut self, ui: &mut Ui) {
        let Some(action) = self.pending_arma3_profile_action.clone() else {
            return;
        };

        let title = match action.kind {
            Arma3ProfileActionKind::Rename => tr("Rename Arma 3 profile"),
            Arma3ProfileActionKind::Clone => tr("Clone Arma 3 profile"),
            Arma3ProfileActionKind::Delete => tr("Delete Arma 3 profile"),
        };

        let mut close_modal = false;
        let mut confirmed = false;
        egui::Window::new(title)
            .frame(
                egui::Frame::window(&ui.ctx().global_style())
                    .fill(self.color_card_bg())
                    .stroke(egui::Stroke::new(1.0, self.color_text_normal()))
                    .corner_radius(eframe::egui::CornerRadius::same(10)),
            )
            .title_bar(true)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .default_width(520.0)
            .show(ui.ctx(), |ui| {
                ui.vertical_centered(|ui| {
                    ui.label(self.t_fmt(
                        "Profile: {name}",
                        &[("name", action.profile.name.clone())],
                    ));
                    ui.label(
                        RichText::new(action.profile.path.display().to_string())
                            .small()
                            .color(self.color_text_dim()),
                    );
                    ui.add_space(10.0);

                    let mut name_error: Option<String> = None;
                    match action.kind {
                        Arma3ProfileActionKind::Rename | Arma3ProfileActionKind::Clone => {
                            let mut name_input = action.name_input.clone();
                            ui.label(tr("New profile name"));
                            let name_edit = ui.add(
                                TextEdit::singleline(&mut name_input).desired_width(300.0),
                            );
                            if name_edit.changed()
                                && let Some(pending) = self.pending_arma3_profile_action.as_mut()
                            {
                                pending.name_input = name_input.clone();
                            }
                            name_error = self.arma3_profile_name_error(&action, &name_input);
                            if let Some(error) = &name_error {
                                ui.label(
                                    RichText::new(error.clone())
                                        .small()
                                        .color(self.color_text_error()),
                                );
                            }
                        }
                        Arma3ProfileActionKind::Delete => {
                            ui.label(tr(
                                "This removes the profile with its settings and keybinds from Arma 3.",
                            ));
                            ui.label(
                                RichText::new(tr(
                                    "The profile folder is moved to the Foxy backup directory, so it can be restored manually if needed.",
                                ))
                                .small()
                                .color(self.color_text_dim()),
                            );
                        }
                    }

                    ui.add_space(20.0);
                    let confirm_label = match action.kind {
                        Arma3ProfileActionKind::Rename => tr("Rename"),
                        Arma3ProfileActionKind::Clone => tr("Clone"),
                        Arma3ProfileActionKind::Delete => tr("Delete profile"),
                    };
                    let confirm_enabled = name_error.is_none();
                    let confirm_button = if action.kind == Arma3ProfileActionKind::Delete {
                        ui.add(Button::new(confirm_label).fill(self.color_text_error()))
                    } else {
                        ui.add_enabled(confirm_enabled, Button::new(confirm_label))
                    };
                    if confirm_button.hovered() && confirm_enabled {
                        ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                    }
                    if confirm_button.clicked() {
                        confirmed = true;
                        close_modal = true;
                    }

                    let cancel_button = ui.button(tr("Cancel"));
                    if cancel_button.hovered() {
                        ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                    }
                    if cancel_button.clicked() {
                        close_modal = true;
                    }
                });
            });

        if confirmed {
            // Re-read the (possibly edited) name from state before executing.
            if let Some(action) = self.pending_arma3_profile_action.clone() {
                self.execute_arma3_profile_action(&action);
            }
        }
        if close_modal {
            self.pending_arma3_profile_action = None;
        }
    }

    /// Validation message for a proposed profile name, or None when valid.
    fn arma3_profile_name_error(
        &self,
        action: &Arma3ProfileAction,
        name_input: &str,
    ) -> Option<String> {
        let name = name_input.trim();
        if action.kind == Arma3ProfileActionKind::Rename && name == action.profile.name {
            return Some(self.t("Enter a different name."));
        }
        match validate_new_profile_name(name) {
            Ok(()) => {
                let duplicate = self
                    .detected_arma3_profiles
                    .iter()
                    .any(|profile| profile.name.eq_ignore_ascii_case(name));
                if duplicate {
                    Some(self.t("A profile with this name already exists."))
                } else {
                    None
                }
            }
            Err(ProfileNameError::Empty) => Some(self.t("Profile name cannot be empty.")),
            Err(ProfileNameError::TooLong) => Some(self.t("Profile name is too long.")),
            Err(ProfileNameError::UnsupportedCharacters) => Some(self.t(
                "Profile name contains unsupported characters. Allowed: letters, numbers, spaces, - _ [ ]",
            )),
            Err(ProfileNameError::Reserved) => {
                Some(self.t("This name is reserved by the operating system."))
            }
        }
    }

    /// Directories that must never be renamed or deleted as a whole:
    /// the vanilla profile roots and the configured custom profiles root.
    fn arma3_profile_protected_roots(&self) -> Vec<PathBuf> {
        let mut roots = Vec::new();
        if let Some(documents) = dirs::document_dir() {
            roots.push(documents.join("Arma 3"));
            roots.push(documents.join("Arma 3 - Other Profiles"));
        }
        let custom_dir = self.settings_view_state.arma3_profiles_directory.trim();
        if !custom_dir.is_empty() {
            let custom_dir = PathBuf::from(custom_dir);
            roots.push(custom_dir.join("Users"));
            roots.push(custom_dir);
        }
        roots
    }

    fn execute_arma3_profile_action(&mut self, action: &Arma3ProfileAction) {
        if is_arma3_running() {
            warn!("Refusing Arma 3 profile management action while the game is running");
            self.show_error_toast(
                self.t("Arma 3 is currently running. Close the game before managing profiles."),
            );
            return;
        }

        let protected_roots = self.arma3_profile_protected_roots();
        let new_name = action.name_input.trim().to_string();
        let result = match action.kind {
            Arma3ProfileActionKind::Rename => {
                rename_profile(&action.profile, &new_name, &protected_roots).map(|_| {
                    let old_name = action.profile.name.clone();
                    let mut repos_updated = false;
                    for repo in &mut self.repository_view_state.repositories {
                        if repo.arma3_profile.as_deref() == Some(old_name.as_str()) {
                            repo.arma3_profile = Some(new_name.clone());
                            repos_updated = true;
                        }
                    }
                    if repos_updated {
                        self.save_repositories();
                    }
                    self.t_fmt("Profile renamed to {name}.", &[("name", new_name.clone())])
                })
            }
            Arma3ProfileActionKind::Clone => {
                let fallback_root = other_profiles_root()
                    .unwrap_or_else(|| app_paths::foxy_backups_dir().join("arma3_profiles"));
                clone_profile(&action.profile, &new_name, &fallback_root, &protected_roots)
                    .map(|_| self.t_fmt("Profile cloned to {name}.", &[("name", new_name.clone())]))
            }
            Arma3ProfileActionKind::Delete => {
                let trash_root = self
                    .configured_backup_directory()
                    .unwrap_or_else(app_paths::foxy_backups_dir)
                    .join("deleted_arma3_profiles");
                delete_profile(&action.profile, &trash_root, &protected_roots).map(|moved_to| {
                    self.t_fmt(
                        "Profile deleted. A backup copy was moved to {path}.",
                        &[("path", moved_to.display().to_string())],
                    )
                })
            }
        };

        match result {
            Ok(message) => self.show_success_toast(message),
            Err(error) => {
                warn!("Arma 3 profile management action failed: {}", error);
                let message = match &error {
                    ProfileOpError::InvalidName(_) => {
                        self.t("The entered profile name is not valid.")
                    }
                    ProfileOpError::TargetAlreadyExists => {
                        self.t("A profile with this name already exists.")
                    }
                    ProfileOpError::SourceMissing => {
                        self.t("Profile files were not found on disk.")
                    }
                    ProfileOpError::DefaultProfileProtected => {
                        self.t("The default Arma 3 profile cannot be deleted.")
                    }
                    ProfileOpError::UnsafePath => {
                        self.t("This profile location cannot be modified safely.")
                    }
                    ProfileOpError::Io(io_error) => self.t_fmt(
                        "Profile operation failed: {error}",
                        &[("error", io_error.to_string())],
                    ),
                };
                self.show_error_toast(message);
            }
        }

        self.refresh_detected_arma3_profiles();
    }
}
