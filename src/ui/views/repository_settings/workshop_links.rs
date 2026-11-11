use std::path::Path;
use std::sync::mpsc::{self, TryRecvError};

use crate::core::game::registry;
use crate::ui::app::Foxy;
use crate::ui::i18n::tr;
use eframe::egui;

impl Foxy {
    pub(super) fn addon_workshop_label(&self) -> Option<String> {
        registry()
            .active_module()?
            .addon_workshop(&self.settings_view_state)
            .map(|workshop| tr(workshop.label()))
    }

    pub(super) fn open_addon_workshop(
        &mut self,
        ctx: &egui::Context,
        addon_name: &str,
        local_path: Option<&str>,
        repo_index: Option<usize>,
    ) {
        let Some(workshop) = registry()
            .active_module()
            .and_then(|module| module.addon_workshop(&self.settings_view_state))
        else {
            return;
        };
        let addon_name = addon_name.to_string();
        let local_path = local_path.map(str::to_owned);
        let repository_url = repo_index
            .and_then(|index| self.repository_view_state.repositories.get(index))
            .map(|repo| repo.address.clone());
        let (tx, rx) = mpsc::channel();
        self.addon_workshop_lookups.push(rx);
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let url = workshop.resolve(
                &addon_name,
                local_path.as_deref().map(Path::new),
                repository_url.as_deref(),
            );
            if tx.send(url).is_ok() {
                Foxy::request_background_repaint(Some(&ctx));
            }
        });
    }

    pub(crate) fn poll_addon_workshop_lookups(&mut self, ctx: &egui::Context) {
        let mut results = Vec::new();
        self.addon_workshop_lookups
            .retain(|rx| match rx.try_recv() {
                Ok(url) => {
                    results.push(url);
                    false
                }
                Err(TryRecvError::Empty) => true,
                Err(TryRecvError::Disconnected) => {
                    results.push(None);
                    false
                }
            });
        for result in results {
            if let Some(url) = result {
                ctx.open_url(egui::OpenUrl::new_tab(url));
            } else {
                self.show_error_toast(tr("Could not find this addon's Workshop page."));
            }
        }
    }
}
