pub mod tasks;

use arboard::Clipboard;
use eframe::egui::{self, Button, Margin, RichText, ScrollArea, TextEdit, Ui, Vec2};
use log::warn;

use crate::core::game::workshop::checksum::{self, StateChecksum};
use crate::core::game::workshop::pin::{PinState, PinStatus};
use crate::core::game::workshop::share::{self, ShareCodeOptions, ShareComparison};
use crate::core::game::{spaces, workshop};
use crate::ui::app::Foxy;
use crate::ui::i18n::tr;

use tasks::{WorkshopTask, WorkshopTaskContext, WorkshopTaskOutcome};

#[derive(Default)]
pub struct WorkshopViewState {
    pub items: Vec<PinStatus>,
    pub load_error: Option<String>,
    pub status: Option<String>,
    pub busy_label: Option<&'static str>,
    pub checksum: Option<StateChecksum>,
    pub filter: String,
    pub show_import_modal: bool,
    pub import_text: String,
    pub import_download: bool,
    pub import_freeze: bool,
    pub pending_remove: Option<String>,
    pub remove_delete_data: bool,
    pub compare_code: String,
    pub compare_result: Option<ShareComparison>,
    /// Cleared whenever the tab is (re)entered so the store is read from disk
    /// on the next frame. The CLI and other Foxy processes can change
    /// `workshop.json` while the tab sits idle.
    pub loaded: bool,
}

impl Foxy {
    /// The Steam app id whose Workshop store this space manages, or `None` when
    /// the active game has no Workshop support or has not been configured yet.
    pub(crate) fn workshop_app_id(&self) -> Option<u32> {
        let module = crate::core::game::registry().active_module()?;
        if !module.capabilities().steam_workshop {
            return None;
        }
        module.steam_app_id_from_settings(&self.settings_view_state)
    }

    pub(crate) fn reload_workshop_view(&mut self) {
        let Some(app_id) = self.workshop_app_id() else {
            self.workshop_view_state.items.clear();
            self.workshop_view_state.checksum = None;
            self.workshop_view_state.load_error = Some(self.t(
                "This game space has no Steam App ID yet. Set one in the game space settings to manage Workshop mods.",
            ));
            return;
        };
        let space_dir = spaces::active_game_space_dir();
        let steam_directory = self.settings_view_state.steam_directory.clone();
        match crate::core::game::workshop::pin::pin_status(&space_dir, app_id, &steam_directory) {
            Ok(items) => {
                self.workshop_view_state.items = items;
                self.workshop_view_state.load_error = None;
            }
            Err(error) => {
                warn!("Failed to read the Workshop store: {}", error);
                self.workshop_view_state.items.clear();
                self.workshop_view_state.load_error = Some(error);
            }
        }
        let game_id = crate::core::game::registry().active().id().to_string();
        self.workshop_view_state.checksum =
            checksum::state_checksum_for_space(&space_dir, &game_id, app_id, &steam_directory, &[])
                .ok();
        self.workshop_view_state.loaded = true;
        self.refresh_workshop_comparison();
    }

    fn refresh_workshop_comparison(&mut self) {
        let code = self.workshop_view_state.compare_code.trim().to_string();
        if code.is_empty() {
            self.workshop_view_state.compare_result = None;
            return;
        }
        let local = self.workshop_share_items(false);
        let remote = share::parse_share_code(&code);
        self.workshop_view_state.compare_result = Some(share::compare_share_lists(&local, &remote));
    }

    fn workshop_share_items(&self, include_disabled: bool) -> Vec<share::SharedItem> {
        self.workshop_view_state
            .items
            .iter()
            .filter(|item| include_disabled || item.enabled)
            .map(|item| share::SharedItem {
                item_id: item.item_id.clone(),
                name: item.title.clone(),
                load_order: item.load_order,
                version: item.pinned_version.clone(),
            })
            .collect()
    }

    pub(crate) fn start_workshop_task(&mut self, task: WorkshopTask) {
        if self.workshop_view_state.busy_label.is_some() {
            self.show_error_toast(self.t("Another Workshop action is still running."));
            return;
        }
        let Some(app_id) = self.workshop_app_id() else {
            self.show_error_toast(self.t("This game space has no Steam App ID yet."));
            return;
        };
        let ctx = WorkshopTaskContext {
            app_id,
            game_id: crate::core::game::registry().active().id().to_string(),
            steam_directory: self.settings_view_state.steam_directory.clone(),
            timeout_seconds: self.workshop_task_timeout_seconds(),
        };
        let (result_tx, result_rx) = std::sync::mpsc::channel::<WorkshopTaskOutcome>();
        let repaint_ctx = self.repaint_ctx.clone();
        self.workshop_view_state.busy_label = Some(task.busy_label());
        self.workshop_view_state.status = None;
        self.workshop_task_rx = Some(result_rx);
        self.workshop_task_worker = Some(std::thread::spawn(move || {
            tasks::run_workshop_task(task, ctx, result_tx, repaint_ctx);
        }));
        self.needs_repaint = true;
    }

    pub(crate) fn poll_workshop_task(&mut self) {
        let Some(rx) = self.workshop_task_rx.as_ref() else {
            return;
        };
        let outcome = match rx.try_recv() {
            Ok(outcome) => outcome,
            Err(std::sync::mpsc::TryRecvError::Empty) => return,
            // The worker died without reporting; clearing the busy flag is what
            // keeps the view from waiting on a result that can never arrive.
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.workshop_task_rx = None;
                self.workshop_task_worker = None;
                self.workshop_view_state.busy_label = None;
                self.workshop_view_state.status =
                    Some(self.t("The Workshop action stopped unexpectedly."));
                self.reload_workshop_view();
                return;
            }
        };
        self.workshop_task_rx = None;
        self.workshop_task_worker = None;
        self.workshop_view_state.busy_label = None;
        match outcome.message {
            Ok(message) => {
                self.workshop_view_state.status = Some(message.clone());
                self.show_success_toast(message);
            }
            Err(error) => {
                self.workshop_view_state.status = Some(error.clone());
                self.show_error_toast(error);
            }
        }
        self.reload_workshop_view();
        self.needs_repaint = true;
    }

    /// The Steam Workshop tab of the game space settings view. The store is
    /// keyed by the active game space, so another space can only be told to
    /// open itself first.
    pub(crate) fn render_workshop_tab(&mut self, ui: &mut Ui, is_active_space: bool) {
        if !is_active_space {
            ui.label(
                RichText::new(tr("Open this game space to manage Steam Workshop mods."))
                    .italics()
                    .color(self.color_text_dim()),
            );
            return;
        }
        if !self.workshop_view_state.loaded {
            self.reload_workshop_view();
        }
        ui.vertical(|ui| {
            self.render_workshop_toolbar(ui);
            ui.separator();
            self.render_workshop_comparison(ui);
            self.render_workshop_body(ui);
        });
    }

    /// Rendered outside the settings card so the modals are not clipped by it.
    pub(crate) fn render_workshop_modals(&mut self, ui: &mut Ui) {
        self.render_workshop_import_modal(ui);
        self.render_workshop_remove_confirmation(ui);
    }

    fn render_workshop_state_badge(&mut self, ui: &mut Ui) {
        if let Some(busy) = self.workshop_view_state.busy_label {
            ui.spinner();
            ui.label(RichText::new(tr(busy)).color(self.color_text_dim()));
            return;
        }
        let Some(checksum) = self.workshop_view_state.checksum.as_ref() else {
            return;
        };
        let code = checksum.checksum.clone();
        let mod_count = checksum.mods.len();
        let badge = ui
            .button(RichText::new(format!("# {}", code)).monospace())
            .on_hover_text(self.t_fmt(
                "State checksum over {count} enabled mod(s), the game build, and load order. Click to copy; a friend with the same code is running the same setup.",
                &[("count", mod_count.to_string())],
            ));
        if badge.hovered() {
            ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
        }
        if badge.clicked() {
            self.copy_to_clipboard(code, "State checksum copied.");
        }
    }

    fn render_workshop_toolbar(&mut self, ui: &mut Ui) {
        let busy = self.workshop_view_state.busy_label.is_some();
        ui.horizontal_wrapped(|ui| {
            if ui
                .add_enabled(!busy, Button::new(tr("Import share code")))
                .on_hover_text(tr(
                    "Paste a list of Workshop ids from a friend and download them.",
                ))
                .clicked()
            {
                self.workshop_view_state.show_import_modal = true;
                self.workshop_view_state.import_text.clear();
                self.workshop_view_state.import_download = true;
                self.workshop_view_state.import_freeze = false;
            }
            if ui
                .button(tr("Copy share code"))
                .on_hover_text(tr(
                    "Copy the enabled mods as a pipe-separated list other mod managers understand.",
                ))
                .clicked()
            {
                let items = self.workshop_share_items(false);
                if items.is_empty() {
                    self.show_error_toast(self.t("There are no enabled Workshop mods to share."));
                } else {
                    let code = share::render_share_code(
                        &items,
                        ShareCodeOptions {
                            include_load_order: true,
                            include_versions: false,
                        },
                    );
                    self.copy_to_clipboard(code, "Share code copied.");
                }
            }
            if ui
                .add_enabled(!busy, Button::new(tr("Freeze all")))
                .on_hover_text(tr(
                    "Keep a private copy of every mod at its current version so a Workshop update cannot change your setup.",
                ))
                .clicked()
            {
                self.start_workshop_task(WorkshopTask::FreezeAll { refresh: false });
            }
            if ui
                .add_enabled(!busy, Button::new(tr("Export bundle")))
                .on_hover_text(tr(
                    "Write a .foxyshare file with the mod list and every frozen copy.",
                ))
                .clicked()
            {
                self.request_workshop_bundle_export();
            }
            if ui
                .add_enabled(!busy, Button::new(tr("Import bundle")))
                .on_hover_text(tr("Read a .foxyshare file a friend sent you."))
                .clicked()
            {
                self.request_workshop_bundle_import();
            }
            if ui
                .add_enabled(!busy, Button::new(tr("Refresh")))
                .on_hover_text(tr("Fetch titles, sizes, and update times from Steam."))
                .clicked()
            {
                self.start_workshop_task(WorkshopTask::RefreshMetadata);
            }
            self.render_workshop_state_badge(ui);
        });
        ui.horizontal(|ui| {
            ui.label(tr("Filter"));
            let width = (ui.available_width() * 0.4).max(120.0);
            ui.add(
                TextEdit::singleline(&mut self.workshop_view_state.filter)
                    .hint_text(tr("Name or id"))
                    .desired_width(width),
            );
        });
    }

    fn render_workshop_comparison(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            ui.label(tr("Compare with"));
            let width = (ui.available_width() - 90.0).max(120.0);
            let response = ui.add(
                TextEdit::singleline(&mut self.workshop_view_state.compare_code)
                    .hint_text(tr("Paste a friend's share code"))
                    .desired_width(width),
            );
            if response.changed() {
                self.refresh_workshop_comparison();
            }
            if ui.button(tr("Clear")).clicked() {
                self.workshop_view_state.compare_code.clear();
                self.workshop_view_state.compare_result = None;
            }
        });

        let Some(comparison) = self.workshop_view_state.compare_result.clone() else {
            return;
        };
        if comparison.matches() {
            ui.label(
                RichText::new(tr("Your enabled mods match the shared list."))
                    .color(self.color_success()),
            );
            ui.separator();
            return;
        }
        ui.horizontal_wrapped(|ui| {
            if !comparison.missing.is_empty() {
                ui.label(
                    RichText::new(self.t_fmt(
                        "{count} missing",
                        &[("count", comparison.missing.len().to_string())],
                    ))
                    .color(self.color_text_error()),
                );
                if ui
                    .button(tr("Import missing"))
                    .on_hover_text(tr("Download only the mods you do not have yet."))
                    .clicked()
                {
                    self.start_workshop_task(WorkshopTask::Import {
                        items: comparison.missing.clone(),
                        download: true,
                        freeze: false,
                    });
                }
            }
            if !comparison.extra.is_empty() {
                ui.label(self.t_fmt(
                    "{count} extra",
                    &[("count", comparison.extra.len().to_string())],
                ));
            }
            if !comparison.unresolvable.is_empty() {
                ui.label(
                    RichText::new(self.t_fmt(
                        "{count} not on the Workshop",
                        &[("count", comparison.unresolvable.len().to_string())],
                    ))
                    .color(self.color_text_dim()),
                );
            }
            if comparison.order_differs {
                ui.label(RichText::new(tr("Load order differs")).color(self.color_text_dim()));
            }
        });
        ui.separator();
    }

    fn render_workshop_body(&mut self, ui: &mut Ui) {
        if let Some(error) = self.workshop_view_state.load_error.clone() {
            ui.label(RichText::new(error).color(self.color_text_error()));
            return;
        }
        if self.workshop_view_state.items.is_empty() {
            ui.label(
                RichText::new(tr(
                    "No Steam Workshop mods yet. Use Import share code to add some.",
                ))
                .italics()
                .color(self.color_text_dim()),
            );
            return;
        }

        let filter = self.workshop_view_state.filter.trim().to_lowercase();
        let items = self.workshop_view_state.items.clone();
        let busy = self.workshop_view_state.busy_label.is_some();
        let last_index = items.len().saturating_sub(1);

        let mut toggle: Option<(String, bool)> = None;
        let mut move_request: Option<(usize, usize)> = None;
        let mut freeze_request: Option<String> = None;
        let mut unfreeze_request: Option<String> = None;
        let mut remove_request: Option<String> = None;
        let mut open_page: Option<String> = None;

        ScrollArea::vertical()
            .id_salt("workshop_items")
            .show(ui, |ui| {
                for (index, item) in items.iter().enumerate() {
                    let title = item.title.clone().unwrap_or_else(|| item.item_id.clone());
                    if !filter.is_empty()
                        && !title.to_lowercase().contains(&filter)
                        && !item.item_id.contains(&filter)
                    {
                        continue;
                    }
                    egui::Frame::NONE
                        .fill(self.color_card_bg())
                        .corner_radius(egui::CornerRadius::same(8))
                        .inner_margin(Margin::symmetric(12, 8))
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                let mut enabled = item.enabled;
                                if ui.checkbox(&mut enabled, "").changed() {
                                    toggle = Some((item.item_id.clone(), enabled));
                                }
                                ui.vertical(|ui| {
                                    ui.horizontal(|ui| {
                                        ui.label(
                                            RichText::new(&title)
                                                .strong()
                                                .color(self.color_text_normal()),
                                        );
                                        let (label, color) = self.workshop_pin_badge(item.state);
                                        if let Some(label) = label {
                                            ui.label(RichText::new(label).small().color(color));
                                        }
                                    });
                                    ui.label(
                                        RichText::new(format!(
                                            "{}  {}{}",
                                            item.item_id,
                                            tr("order"),
                                            item.load_order
                                                .map(|order| format!(" {}", order))
                                                .unwrap_or_else(|| " -".to_string())
                                        ))
                                        .small()
                                        .color(self.color_text_dim()),
                                    );
                                });
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        if ui
                                            .add_enabled(!busy, Button::new(tr("Remove")))
                                            .clicked()
                                        {
                                            remove_request = Some(item.item_id.clone());
                                        }
                                        if item.frozen {
                                            if ui
                                                .add_enabled(!busy, Button::new(tr("Unfreeze")))
                                                .on_hover_text(tr(
                                                    "Follow Steam updates again for this mod.",
                                                ))
                                                .clicked()
                                            {
                                                unfreeze_request = Some(item.item_id.clone());
                                            }
                                        } else if ui
                                            .add_enabled(!busy, Button::new(tr("Freeze")))
                                            .on_hover_text(tr(
                                                "Pin this mod at its current version.",
                                            ))
                                            .clicked()
                                        {
                                            freeze_request = Some(item.item_id.clone());
                                        }
                                        if !item.url.is_empty()
                                            && ui
                                                .button(tr("Open"))
                                                .on_hover_text(tr("Open the Workshop page."))
                                                .clicked()
                                        {
                                            open_page = Some(item.url.clone());
                                        }
                                        if ui
                                            .add_enabled(
                                                index < last_index,
                                                Button::new("\u{2193}"),
                                            )
                                            .on_hover_text(tr("Load later"))
                                            .clicked()
                                        {
                                            move_request = Some((index, index + 1));
                                        }
                                        if ui
                                            .add_enabled(index > 0, Button::new("\u{2191}"))
                                            .on_hover_text(tr("Load earlier"))
                                            .clicked()
                                        {
                                            move_request = Some((index, index - 1));
                                        }
                                    },
                                );
                            });
                        });
                    ui.add_space(4.0);
                }
            });

        if let Some((item_id, enabled)) = toggle {
            self.set_workshop_item_enabled(&item_id, enabled);
        }
        if let Some((from, to)) = move_request {
            self.move_workshop_item(from, to);
        }
        if let Some(item_id) = freeze_request {
            self.start_workshop_task(WorkshopTask::Freeze { item_id });
        }
        if let Some(item_id) = unfreeze_request {
            self.unfreeze_workshop_item(&item_id);
        }
        if let Some(item_id) = remove_request {
            self.workshop_view_state.pending_remove = Some(item_id);
            self.workshop_view_state.remove_delete_data = false;
        }
        if let Some(url) = open_page {
            ui.ctx().open_url(egui::OpenUrl::new_tab(url));
        }
    }

    fn workshop_pin_badge(&self, state: PinState) -> (Option<String>, egui::Color32) {
        match state {
            PinState::NotFrozen => (None, self.color_text_dim()),
            PinState::InSync => (Some(self.t("frozen")), self.color_success()),
            PinState::Drifted => (
                Some(self.t("frozen - Steam has a newer version")),
                self.color_text_error(),
            ),
            PinState::LiveMissing => (
                Some(self.t("frozen - not installed")),
                self.color_text_dim(),
            ),
            PinState::FrozenMissing => (
                Some(self.t("frozen copy is missing")),
                self.color_text_error(),
            ),
        }
    }

    fn set_workshop_item_enabled(&mut self, item_id: &str, enabled: bool) {
        let Some(app_id) = self.workshop_app_id() else {
            return;
        };
        let space_dir = spaces::active_game_space_dir();
        match workshop::set_item_enabled(&space_dir, app_id, item_id, enabled) {
            Ok(_) => self.reload_workshop_view(),
            Err(error) => {
                warn!("Failed to toggle Workshop item: {}", error);
                self.show_error_toast(error);
            }
        }
    }

    fn unfreeze_workshop_item(&mut self, item_id: &str) {
        let Some(app_id) = self.workshop_app_id() else {
            return;
        };
        let space_dir = spaces::active_game_space_dir();
        match workshop::unfreeze_item(&space_dir, app_id, item_id) {
            Ok(_) => self.reload_workshop_view(),
            Err(error) => {
                warn!("Failed to unfreeze Workshop item: {}", error);
                self.show_error_toast(error);
            }
        }
    }

    /// Reordering rewrites every position, so a store that never had explicit
    /// load order gets a complete one on the first move instead of a single
    /// number nothing else can be compared against.
    fn move_workshop_item(&mut self, from: usize, to: usize) {
        let Some(app_id) = self.workshop_app_id() else {
            return;
        };
        let mut ids = self
            .workshop_view_state
            .items
            .iter()
            .map(|item| item.item_id.clone())
            .collect::<Vec<_>>();
        if from >= ids.len() || to >= ids.len() {
            return;
        }
        let moved = ids.remove(from);
        ids.insert(to, moved);

        let space_dir = spaces::active_game_space_dir();
        for (index, item_id) in ids.iter().enumerate() {
            if let Err(error) =
                workshop::set_item_load_order(&space_dir, app_id, item_id, Some(index as u32 + 1))
            {
                warn!("Failed to set Workshop load order: {}", error);
                self.show_error_toast(error);
                break;
            }
        }
        self.reload_workshop_view();
    }

    fn request_workshop_bundle_export(&mut self) {
        let Some(path) = crate::ui::app::agent_support::save_file(|| {
            rfd::FileDialog::new()
                .add_filter("Foxy share bundle", &["foxyshare"])
                .set_file_name("workshop-share.foxyshare")
                .save_file()
        }) else {
            return;
        };
        self.start_workshop_task(WorkshopTask::ExportBundle {
            path,
            include_disabled: false,
        });
    }

    fn request_workshop_bundle_import(&mut self) {
        let Some(path) = crate::ui::app::agent_support::pick_file(|| {
            rfd::FileDialog::new()
                .add_filter("Foxy share bundle", &["foxyshare"])
                .pick_file()
        }) else {
            return;
        };
        self.start_workshop_task(WorkshopTask::ImportBundle {
            path,
            download: true,
        });
    }

    fn render_workshop_import_modal(&mut self, ui: &mut Ui) {
        if !self.workshop_view_state.show_import_modal {
            return;
        }
        let mut open = true;
        let mut confirm = false;
        egui::Window::new(tr("Import share code"))
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
            .open(&mut open)
            .show(ui.ctx(), |ui| {
                ui.set_min_width(460.0);
                ui.label(
                    RichText::new(tr(
                        "Paste a pipe-separated list of Workshop ids, Workshop links, or one id per line.",
                    ))
                    .italics()
                    .color(self.color_text_dim()),
                );
                ui.add_space(6.0);
                ui.add(
                    TextEdit::multiline(&mut self.workshop_view_state.import_text)
                        .desired_rows(6)
                        .desired_width(f32::INFINITY),
                );
                ui.add_space(6.0);
                ui.checkbox(
                    &mut self.workshop_view_state.import_download,
                    tr("Subscribe and download through Steam"),
                );
                ui.checkbox(
                    &mut self.workshop_view_state.import_freeze,
                    tr("Freeze each mod after downloading"),
                );
                let parsed = share::parse_share_code(&self.workshop_view_state.import_text);
                let resolvable = parsed.iter().filter(|item| item.is_resolvable()).count();
                ui.add_space(6.0);
                ui.label(self.t_fmt(
                    "{count} Workshop mod(s) recognized",
                    &[("count", resolvable.to_string())],
                ));
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(resolvable > 0, Button::new(tr("Import")))
                        .clicked()
                    {
                        confirm = true;
                    }
                    if ui.button(tr("Cancel")).clicked() {
                        self.workshop_view_state.show_import_modal = false;
                    }
                });
            });
        if !open {
            self.workshop_view_state.show_import_modal = false;
        }
        if confirm {
            let items = share::parse_share_code(&self.workshop_view_state.import_text);
            let download = self.workshop_view_state.import_download;
            let freeze = self.workshop_view_state.import_freeze;
            self.workshop_view_state.show_import_modal = false;
            self.start_workshop_task(WorkshopTask::Import {
                items,
                download,
                freeze,
            });
        }
    }

    fn render_workshop_remove_confirmation(&mut self, ui: &mut Ui) {
        let Some(item_id) = self.workshop_view_state.pending_remove.clone() else {
            return;
        };
        let mut open = true;
        let mut confirm = false;
        egui::Window::new(tr("Remove Workshop mod"))
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
            .open(&mut open)
            .show(ui.ctx(), |ui| {
                ui.set_min_width(380.0);
                ui.label(self.t_fmt(
                    "Remove {item} from this game space?",
                    &[("item", item_id.clone())],
                ));
                ui.checkbox(
                    &mut self.workshop_view_state.remove_delete_data,
                    tr("Also delete the downloaded files and frozen copies"),
                );
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if ui.button(tr("Remove")).clicked() {
                        confirm = true;
                    }
                    if ui.button(tr("Cancel")).clicked() {
                        self.workshop_view_state.pending_remove = None;
                    }
                });
            });
        if !open {
            self.workshop_view_state.pending_remove = None;
        }
        if confirm {
            let delete_data = self.workshop_view_state.remove_delete_data;
            self.workshop_view_state.pending_remove = None;
            self.start_workshop_task(WorkshopTask::Remove {
                item_id,
                delete_data,
            });
        }
    }

    fn copy_to_clipboard(&mut self, value: String, success_message: &str) {
        match Clipboard::new().and_then(|mut clipboard| clipboard.set_text(value)) {
            Ok(()) => self.show_success_toast(self.t(success_message)),
            Err(error) => {
                warn!("Failed to copy to clipboard: {}", error);
                self.show_error_toast(self.t("Failed to copy to clipboard."));
            }
        }
    }
}
