use egui::{Key, Modifiers};
use log::info;

use crate::ui::app::{Foxy, FoxyView};
use crate::ui::types::{AboutTab, HelpTab};

impl Foxy {
    pub fn is_reference_view(view: FoxyView) -> bool {
        matches!(
            view,
            FoxyView::Help
                | FoxyView::Changelog
                | FoxyView::About
                | FoxyView::AppUpdate
                | FoxyView::VersionBrowser
        )
    }

    pub fn open_reference_view(&mut self, view: FoxyView) {
        if !Self::is_reference_view(self.current_view) {
            self.last_view = self.current_view;
        } else if self.last_view == FoxyView::None {
            self.last_view = FoxyView::RepositoryList;
        }

        if view == FoxyView::About {
            self.current_about_tab = AboutTab::About;
        }

        self.current_view = view;
    }

    pub fn close_reference_view(&mut self) {
        self.current_view = match self.last_view {
            FoxyView::None => FoxyView::RepositoryList,
            view if Self::is_reference_view(view) => FoxyView::RepositoryList,
            view => view,
        };
        self.last_view = FoxyView::None;
    }

    pub fn restore_last_view_or_default(&mut self) {
        self.current_view = match self.last_view {
            FoxyView::None => FoxyView::RepositoryList,
            view => view,
        };
        self.last_view = FoxyView::None;
    }

    pub fn open_settings_view(&mut self) {
        if self.current_view == FoxyView::Settings {
            self.restore_last_view_or_default();
            return;
        }

        self.last_view = if Foxy::is_reference_view(self.current_view) {
            match self.last_view {
                FoxyView::None => FoxyView::RepositoryList,
                view => view,
            }
        } else {
            self.current_view
        };
        self.settings_view_state.current_tab = "Application".to_string();
        self.current_view = FoxyView::Settings;
    }

    pub(in crate::ui::app) fn handle_global_accessibility_shortcuts(
        &mut self,
        ctx: &egui::Context,
    ) {
        let escape_pressed = ctx.input_mut(|input| input.consume_key(Modifiers::NONE, Key::Escape));
        if escape_pressed {
            if self.update_modal_open {
                self.update_modal_open = false;
                self.direct_download_update_view = false;
                info!("Closed update modal from Escape shortcut");
                return;
            }
            if self.pending_mission_duplicate.is_some() {
                self.pending_mission_duplicate = None;
                info!("Closed mission duplicate modal from Escape shortcut");
                return;
            }
            if self.pending_mission_delete.is_some() {
                self.pending_mission_delete = None;
                info!("Closed mission delete modal from Escape shortcut");
                return;
            }
            if self.pending_mission_editor_launch_warning.is_some() {
                self.pending_mission_editor_launch_warning = None;
                info!("Closed mission editor launch warning from Escape shortcut");
                return;
            }
            if self.pending_join_preflight.is_some() {
                self.pending_join_preflight = None;
                info!("Closed join addon preflight from Escape shortcut");
                return;
            }
            if self.pending_join_preflight_query.is_some() {
                self.pending_join_preflight_query = None;
                info!("Canceled join addon preflight query from Escape shortcut");
                return;
            }

            if Self::is_reference_view(self.current_view) {
                self.close_reference_view();
                info!("Closed reference view from Escape shortcut");
                return;
            }

            if self.show_memory_diagnostics_window {
                self.show_memory_diagnostics_window = false;
                info!("Closed memory diagnostics window from Escape shortcut");
                return;
            }
        }

        if ctx.egui_wants_keyboard_input() {
            return;
        }

        if ctx.input_mut(|input| input.consume_key(Modifiers::NONE, Key::F1)) {
            self.current_help_tab = HelpTab::Overview;
            self.open_reference_view(FoxyView::Help);
            info!("Opened help view from F1 shortcut");
        }

        if ctx.input_mut(|input| input.consume_key(Modifiers::NONE, Key::F2)) {
            self.open_settings_view();
            info!("Toggled settings view from F2 shortcut");
        }

        if ctx.input_mut(|input| input.consume_key(Modifiers::NONE, Key::F3)) {
            self.set_activity_log_visibility(
                ctx,
                !self.settings_view_state.show_activity_log,
                "F3 shortcut",
            );
        }

        if ctx.input_mut(|input| input.consume_key(Modifiers::NONE, Key::F4))
            && self.settings_view_state.show_memory_diagnostics_icon
        {
            self.show_memory_diagnostics_window = !self.show_memory_diagnostics_window;
            if self.show_memory_diagnostics_window {
                self.capture_memory_diagnostics_snapshot("window opened", true);
            }
            info!(
                "Memory diagnostics window visibility set to {} from F4 shortcut",
                self.show_memory_diagnostics_window
            );
        }
    }

    pub(crate) fn set_activity_log_visibility(
        &mut self,
        ctx: &egui::Context,
        visible: bool,
        source: &str,
    ) {
        if self.settings_view_state.show_activity_log == visible {
            return;
        }

        self.settings_view_state.show_activity_log = visible;
        if visible {
            self.activity_log_last_poll_at = None;
        }

        ctx.request_discard("activity log panel visibility changed");
        info!(
            "Activity log panel visibility set to {} from {}",
            self.settings_view_state.show_activity_log, source
        );
        self.save_settings();
    }
}
