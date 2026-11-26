use crate::core::server::{AddonSupply, ContentSources, RepositoryProfile};
use crate::ui::app::Foxy;
use crate::ui::app::agent_support;
use crate::ui::app::server::{ServerProfileEditor, ServerTaskAction};
use crate::ui::i18n::{tr, tr_fmt};
use eframe::egui::{self, Ui};
use std::path::PathBuf;

impl Foxy {
    pub(super) fn render_server_addon_profiles(&mut self, ui: &mut Ui) {
        use ServerTaskAction::*;
        let busy = self.server_view.busy();
        let mut editor = std::mem::take(&mut self.server_view.profile_editor);
        let mut action = None;
        ui.heading(tr("Addon profiles"));
        ui.weak(tr(
            "A profile is a named addon set. Assign it to a server from Server loadout.",
        ));
        if let Some(snapshot) = &self.server_view.content_sources {
            let sources = &snapshot.sources;
            let editable = !busy && !self.server_view.dirty();
            let can_begin = editable && editor.profile.is_none() && editor.folder_edit.is_none();
            ui.horizontal_wrapped(|ui| {
                if ui
                    .add_enabled(can_begin, egui::Button::new(tr("Add addon folder")))
                    .on_hover_text(tr(
                        "Register a folder containing addons already on this machine.",
                    ))
                    .clicked()
                {
                    editor = ServerProfileEditor {
                        folder_edit: Some((String::new(), String::new())),
                        base_fingerprint: snapshot.fingerprint.clone(),
                        ..Default::default()
                    };
                }
                if ui
                    .add_enabled(
                        can_begin && !sources.folders.is_empty(),
                        egui::Button::new(tr("New addon profile")),
                    )
                    .on_disabled_hover_text(tr("Add an addon folder before creating a profile."))
                    .clicked()
                {
                    let folder = &sources.folders[0];
                    editor = ServerProfileEditor {
                        profile: Some(RepositoryProfile {
                            name: String::new(),
                            remote_url: String::new(),
                            local_path: folder.path.clone(),
                            folder_id: folder.id.clone(),
                            supply: AddonSupply::AlreadyPresent,
                            addons: Vec::new(),
                        }),
                        base_fingerprint: snapshot.fingerprint.clone(),
                        ..Default::default()
                    };
                }
            });
            if editor.profile.is_none() && editor.folder_edit.is_none() {
                if sources.profiles.is_empty() {
                    ui.label(tr(
                        "Add a folder, then create a profile and choose its addons.",
                    ));
                }
                if let Some((profile, existing)) =
                    choose_profile(ui, sources, &mut editor.selected_profile, editable)
                {
                    editor = ServerProfileEditor {
                        profile: Some(profile),
                        base_fingerprint: snapshot.fingerprint.clone(),
                        existing,
                        ..Default::default()
                    };
                    if existing {
                        action = Some(LoadContentConsumers);
                    }
                }
            }
            if let Some((id, path)) = &mut editor.folder_edit {
                ui.group(|ui| {
                    ui.strong(tr("Addon folder"));
                    ui.horizontal_wrapped(|ui| {
                        let label = ui.label(tr("Name"));
                        ui.text_edit_singleline(id).labelled_by(label.id);
                    });
                    ui.horizontal_wrapped(|ui| {
                        let label = ui.label(tr("Folder path"));
                        ui.text_edit_singleline(path).labelled_by(label.id);
                        if ui.button(tr("Choose folder")).clicked()
                            && let Some(folder) =
                                agent_support::pick_folder(|| rfd::FileDialog::new().pick_folder())
                        {
                            *path = folder.display().to_string();
                        }
                    });
                    if ui
                        .add_enabled(
                            editable && !id.trim().is_empty() && !path.trim().is_empty(),
                            egui::Button::new(tr("Register folder")),
                        )
                        .clicked()
                    {
                        let mut new = sources.clone();
                        match new.add_folder(id.trim(), &PathBuf::from(path.trim())) {
                            Ok(()) => {
                                action = Some(SaveContentSources {
                                    sources: new,
                                    expected_fingerprint: editor.base_fingerprint.clone(),
                                })
                            }
                            Err(error) => editor.error = Some(error.to_string()),
                        }
                    }
                });
            }
            if editor.profile.is_some() {
                if let Some(task) = render_profile_form(
                    ui,
                    &mut editor,
                    sources,
                    self.server_view.addon_candidates.as_ref(),
                    editable,
                ) {
                    action = Some(task);
                }
                if editor.existing
                    && let Some(profile) = &editor.profile
                {
                    ui.label(tr("Profile edits apply to every server that uses this profile. Active deployments keep their reviewed content."));
                    if let Some(report) = &self.server_view.content_consumers {
                        if let Some(entry) = report
                            .sources
                            .iter()
                            .find(|entry| entry.profile == profile.name)
                        {
                            for instance in &entry.instances {
                                ui.label(tr_fmt(
                                    "Used by {name}",
                                    &[("name", instance.display_name.clone())],
                                ));
                            }
                        }
                        if !report.inspection_complete {
                            ui.label(tr("Shared profile usage could not be fully verified."));
                        }
                    }
                }
            }
            if let Some(error) = &editor.error {
                ui.colored_label(self.color_text_error(), tr(error));
            }
            if (editor.profile.is_some() || editor.folder_edit.is_some())
                && ui
                    .add_enabled(!busy, egui::Button::new(tr("Cancel")))
                    .clicked()
            {
                editor = ServerProfileEditor::default();
            }
        } else if (!editor.initialized
            || ui.button(tr("Refresh server content profiles")).clicked())
            && !busy
        {
            editor.initialized = true;
            action = Some(LoadContentSources);
        }
        self.server_view.profile_editor = editor;
        if let Some(action) = action {
            self.start_server_task(action);
        }
    }
}

fn choose_profile(
    ui: &mut Ui,
    sources: &ContentSources,
    selected: &mut String,
    editable: bool,
) -> Option<(RepositoryProfile, bool)> {
    let first = sources.profiles.first()?;
    if !sources
        .profiles
        .iter()
        .any(|profile| profile.name == *selected)
    {
        *selected = first.name.clone();
    }
    ui.group(|ui| {
        ui.horizontal_wrapped(|ui| {
            let label = ui.label(tr("Profile name"));
            egui::ComboBox::from_id_salt("server-content-profile")
                .selected_text(selected.as_str())
                .show_ui(ui, |ui| {
                    for profile in &sources.profiles {
                        ui.selectable_value(selected, profile.name.clone(), &profile.name);
                    }
                })
                .response
                .labelled_by(label.id);
            let profile = sources
                .profiles
                .iter()
                .find(|profile| profile.name == *selected)?;
            ui.label(tr_fmt(
                "{count} selected addons",
                &[("count", profile.addons.len().to_string())],
            ));
            if ui
                .add_enabled(editable, egui::Button::new(tr("Edit profile")))
                .on_hover_text(tr("Change the selected addons in this shared profile."))
                .clicked()
            {
                return Some((profile.clone(), true));
            }
            let local =
                profile.remote_url.is_empty() && profile.supply == AddonSupply::AlreadyPresent;
            if ui
                .add_enabled(editable && local, egui::Button::new(tr("Copy Profile")))
                .on_hover_text(tr(
                    "Create an independent name and addon selection from this local profile.",
                ))
                .on_disabled_hover_text(tr("Copying is available for local profiles."))
                .clicked()
            {
                let mut profile = profile.clone();
                profile.name.clear();
                return Some((profile, false));
            }
            None
        })
        .inner
    })
    .inner
}

fn render_profile_form(
    ui: &mut Ui,
    editor: &mut ServerProfileEditor,
    sources: &ContentSources,
    candidates: Option<&(PathBuf, Vec<String>)>,
    editable: bool,
) -> Option<ServerTaskAction> {
    let profile = editor.profile.as_mut()?;
    let mut action = None;
    let mut changed = false;
    ui.group(|ui| {
        ui.horizontal_wrapped(|ui| {
            let label = ui.label(tr("Profile name"));
            changed |= ui
                .add_enabled(
                    !editor.existing,
                    egui::TextEdit::singleline(&mut profile.name),
                )
                .labelled_by(label.id)
                .changed();
        });
        ui.horizontal_wrapped(|ui| {
            let label = ui.label(tr("Addon folder"));
            ui.add_enabled_ui(!editor.existing, |ui| {
                egui::ComboBox::from_id_salt("server-profile-folder")
                    .selected_text(&profile.folder_id)
                    .show_ui(ui, |ui| {
                        for folder in &sources.folders {
                            if ui
                                .selectable_value(
                                    &mut profile.folder_id,
                                    folder.id.clone(),
                                    &folder.id,
                                )
                                .changed()
                            {
                                profile.local_path = folder.path.clone();
                                profile.addons.clear();
                                changed = true;
                            }
                        }
                    })
                    .response
                    .labelled_by(label.id);
            });
        });
        let scan_path = sources
            .folders
            .iter()
            .find(|folder| folder.id == profile.folder_id)
            .map(|folder| folder.path.clone());
        if ui
            .add_enabled(
                editable && scan_path.is_some(),
                egui::Button::new(tr("Scan addon folders")),
            )
            .on_hover_text(tr(
                "Find addon directories in this registered folder without changing its files.",
            ))
            .clicked()
        {
            action = scan_path.clone().map(ServerTaskAction::ScanAddonFolder);
        }
        let label = ui.label(tr("Filter addons"));
        ui.text_edit_singleline(&mut editor.filter)
            .labelled_by(label.id);
        let filter = editor.filter.trim().to_lowercase();
        if let Some((_, names)) = candidates.filter(|(path, _)| Some(path) == scan_path.as_ref()) {
            let names = names
                .iter()
                .filter(|name| name.to_lowercase().contains(&filter))
                .collect::<Vec<_>>();
            egui::ScrollArea::vertical()
                .id_salt("server-profile-candidates")
                .max_height(170.0)
                .show_rows(
                    ui,
                    ui.spacing().interact_size.y + 6.0,
                    names.len(),
                    |ui, range| {
                        for name in &names[range] {
                            let position = profile
                                .addons
                                .iter()
                                .position(|item| item.eq_ignore_ascii_case(name));
                            let mut selected = position.is_some();
                            let changed_row = egui::Frame::group(ui.style())
                                .inner_margin(egui::Margin::symmetric(8, 2))
                                .show(ui, |ui| {
                                    ui.set_min_width(ui.available_width());
                                    Foxy::ui_state_checkbox(ui, &mut selected, name.as_str())
                                        .changed()
                                })
                                .inner;
                            if changed_row {
                                if let Some(index) = position {
                                    profile.addons.remove(index);
                                } else {
                                    profile.addons.push((*name).clone());
                                }
                                changed = true;
                            }
                        }
                    },
                );
        }
        ui.horizontal_wrapped(|ui| {
            let label = ui.label(tr("Addon folder name"));
            ui.text_edit_singleline(&mut editor.addon_name)
                .labelled_by(label.id);
            let name = editor.addon_name.trim();
            if ui
                .add_enabled(
                    !name.is_empty()
                        && !profile
                            .addons
                            .iter()
                            .any(|item| item.eq_ignore_ascii_case(name)),
                    egui::Button::new(tr("Add addon")),
                )
                .on_hover_text(tr(
                    "Add a directory by name, including one that will be acquired later.",
                ))
                .clicked()
            {
                profile.addons.push(name.to_owned());
                editor.addon_name.clear();
                changed = true;
            }
        });
        ui.strong(tr_fmt(
            "{count} selected addons",
            &[("count", profile.addons.len().to_string())],
        ));
        let mut remove = None;
        for (index, name) in profile.addons.iter().enumerate() {
            ui.push_id(name, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.label(name);
                    if ui
                        .small_button(tr("Remove"))
                        .accessible_name(tr_fmt("Remove {name}", &[("name", name.clone())]))
                        .clicked()
                    {
                        remove = Some(index);
                    }
                });
            });
        }
        if let Some(index) = remove {
            profile.addons.remove(index);
            changed = true;
        }
    });
    if changed {
        editor.review = None;
        editor.error = None;
    }
    if ui
        .add_enabled(
            editable && !profile.name.trim().is_empty() && !profile.addons.is_empty(),
            egui::Button::new(tr("Review addon profile")),
        )
        .clicked()
    {
        let mut updated = sources.clone();
        let mut draft = profile.clone();
        draft.name = draft.name.trim().to_owned();
        if editor.existing {
            updated.profiles.retain(|entry| entry.name != draft.name);
        }
        match updated.add_profile(draft) {
            Ok(()) => {
                editor.review = Some(updated);
                editor.error = None;
            }
            Err(error) => editor.error = Some(error.to_string()),
        }
    }
    if let Some(reviewed) = &editor.review {
        ui.label(tr(
            "Review the selected folders before saving this addon profile.",
        ));
        if ui
            .add_enabled(editable, egui::Button::new(tr("Save addon profile")))
            .clicked()
        {
            action = Some(ServerTaskAction::SaveContentSources {
                sources: reviewed.clone(),
                expected_fingerprint: editor.base_fingerprint.clone(),
            });
        }
    }
    action
}
