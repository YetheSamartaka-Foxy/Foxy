use crate::core::server::{ServiceExportFormat, ServiceExportPlan};
use crate::ui::app::Foxy;

use super::tasks::ServerTaskAction;

#[derive(Default)]
pub(crate) struct ServerServiceExportViewState {
    pub path: String,
    pub plan: Option<ServiceExportPlan>,
    pub exported: bool,
    pub controls_focus: Option<egui::Id>,
    space_id: Option<String>,
}

impl ServerServiceExportViewState {
    pub fn reset_for_space(&mut self, space_id: &str) {
        self.path.clear();
        self.plan = None;
        self.exported = false;
        self.controls_focus = None;
        self.space_id = Some(space_id.to_owned());
    }

    pub fn plan_for_space(&self, space_id: &str) -> Option<&ServiceExportPlan> {
        self.plan
            .as_ref()
            .filter(|plan| plan.game_space_id == space_id)
    }

    pub fn ensure_space(&mut self, space_id: &str) {
        if self.space_id.as_deref() != Some(space_id) {
            self.reset_for_space(space_id);
        }
    }
}

impl Foxy {
    pub(crate) fn request_server_service_export_plan(&mut self) {
        let space_id = crate::core::game::spaces::active_game_space().space_id;
        self.server_view.services.ensure_space(&space_id);
        self.start_server_task(ServerTaskAction::PlanServiceExport);
    }

    pub(crate) fn export_reviewed_server_service(&mut self) {
        let space_id = crate::core::game::spaces::active_game_space().space_id;
        let Some(plan) = self.server_view.services.plan_for_space(&space_id).cloned() else {
            return;
        };
        let path = self.server_view.services.path.trim();
        if path.is_empty() {
            return;
        }
        self.server_view.services.exported = false;
        self.start_server_task(ServerTaskAction::WriteServiceExport {
            plan,
            path: std::path::PathBuf::from(path),
        });
    }

    pub(crate) fn server_service_export_format() -> ServiceExportFormat {
        if cfg!(target_os = "windows") {
            ServiceExportFormat::WindowsUserTask
        } else {
            ServiceExportFormat::SystemdUser
        }
    }
}
