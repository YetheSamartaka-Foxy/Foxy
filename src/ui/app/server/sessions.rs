use super::tasks::ServerTaskAction;
use crate::core::server::{ReforgerSessionIntent, ReforgerSessionInventory, ReforgerSessionPlan};
use crate::ui::app::Foxy;

#[derive(Default)]
pub(crate) struct ServerSessionsViewState {
    pub inventory: Option<Box<ReforgerSessionInventory>>,
    pub plan: Option<Box<ReforgerSessionPlan>>,
    pub confirmation: bool,
    pub focus_cancel: bool,
    pub mode: String,
    pub save_id: String,
    pub controls_focus: Option<egui::Id>,
}

impl ServerSessionsViewState {
    pub fn reset_for_instance(&mut self) {
        self.inventory = None;
        self.plan = None;
        self.confirmation = false;
        self.focus_cancel = false;
        self.mode.clear();
        self.save_id.clear();
        self.controls_focus = None;
    }

    pub fn intent(&self) -> Option<ReforgerSessionIntent> {
        match self.mode.as_str() {
            "configured" => Some(ReforgerSessionIntent::Configured),
            "latest" => Some(ReforgerSessionIntent::Latest),
            "selected" if !self.save_id.trim().is_empty() => {
                Some(ReforgerSessionIntent::Selected {
                    save_id: self.save_id.trim().to_owned(),
                })
            }
            "new" => Some(ReforgerSessionIntent::New),
            _ => None,
        }
    }
}

impl Foxy {
    pub(crate) fn request_server_sessions(&mut self) {
        let Some(id) = self
            .server_view
            .editor
            .as_ref()
            .map(|editor| editor.baseline.id.clone())
        else {
            return;
        };
        self.server_view.sessions.plan = None;
        self.server_view.sessions.confirmation = false;
        self.start_server_task(ServerTaskAction::LoadServerSessions(id));
    }

    pub(crate) fn request_server_session_plan(&mut self) {
        let Some(editor) = self.server_view.editor.as_ref() else {
            return;
        };
        let id = editor.baseline.id.clone();
        if editor.dirty() {
            return;
        }
        let Some(intent) = self.server_view.sessions.intent() else {
            return;
        };
        let Some(inventory) = self
            .server_view
            .sessions
            .inventory
            .as_ref()
            .filter(|inventory| inventory.instance_id == id)
        else {
            return;
        };
        self.server_view.sessions.plan = None;
        self.server_view.sessions.confirmation = false;
        self.start_server_task(ServerTaskAction::PlanServerSession {
            id,
            intent,
            expected_generation: inventory.generation,
        });
    }

    pub(crate) fn apply_server_session_plan(&mut self) {
        let Some(editor) = self.server_view.editor.as_ref() else {
            return;
        };
        if editor.dirty() {
            return;
        }
        let id = editor.baseline.id.clone();
        let Some(plan) = self
            .server_view
            .sessions
            .plan
            .as_ref()
            .filter(|plan| {
                plan.instance_id == id
                    && plan.blockers.is_empty()
                    && self.server_view.sessions.intent().as_ref() == Some(&plan.intent)
            })
            .cloned()
        else {
            return;
        };
        self.server_view.sessions.confirmation = false;
        self.start_server_task(ServerTaskAction::ApplyServerSession { plan });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_intent_maps_each_supported_mode() {
        let mut state = ServerSessionsViewState {
            mode: "configured".into(),
            ..Default::default()
        };
        assert_eq!(state.intent(), Some(ReforgerSessionIntent::Configured));

        state.mode = "latest".into();
        assert_eq!(state.intent(), Some(ReforgerSessionIntent::Latest));

        state.mode = "new".into();
        assert_eq!(state.intent(), Some(ReforgerSessionIntent::New));

        state.mode = "selected".into();
        state.save_id = " save-uuid ".into();
        assert_eq!(
            state.intent(),
            Some(ReforgerSessionIntent::Selected {
                save_id: "save-uuid".into()
            })
        );
    }

    #[test]
    fn selected_session_requires_a_nonblank_uuid_and_unknown_modes_fail() {
        let mut state = ServerSessionsViewState {
            mode: "selected".into(),
            save_id: " \t ".into(),
            ..Default::default()
        };
        assert_eq!(state.intent(), None);
        state.mode = "other".into();
        assert_eq!(state.intent(), None);
    }

    #[test]
    fn resetting_session_state_clears_review_and_selection() {
        let mut state = ServerSessionsViewState {
            confirmation: true,
            focus_cancel: true,
            mode: "selected".into(),
            save_id: "save-uuid".into(),
            ..Default::default()
        };
        state.reset_for_instance();
        assert!(state.plan.is_none());
        assert!(!state.confirmation);
        assert!(!state.focus_cancel);
        assert!(state.mode.is_empty());
        assert!(state.save_id.is_empty());
    }
}
