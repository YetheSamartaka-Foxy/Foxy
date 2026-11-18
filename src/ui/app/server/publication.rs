use crate::core::server::{PlayerPublicationOutcome, PlayerPublicationPlan};

pub(crate) struct PlayerPublicationViewState {
    pub output: String,
    pub repository_name: String,
    pub recommendations: Vec<String>,
    pub include_public_keys: bool,
    pub arma_permission: bool,
    pub plan: Option<PlayerPublicationPlan>,
    pub outcome: Option<PlayerPublicationOutcome>,
}

impl Default for PlayerPublicationViewState {
    fn default() -> Self {
        Self {
            output: String::new(),
            repository_name: "Player content".into(),
            recommendations: Vec::new(),
            include_public_keys: false,
            arma_permission: false,
            plan: None,
            outcome: None,
        }
    }
}

impl PlayerPublicationViewState {
    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

#[cfg(test)]
mod tests {
    use super::PlayerPublicationViewState;

    #[test]
    fn default_and_reset_keep_default_repository_name() {
        let mut state = PlayerPublicationViewState::default();
        assert_eq!(state.repository_name, "Player content");
        state.repository_name.clear();
        state.include_public_keys = true;
        state.reset();
        assert_eq!(state.repository_name, "Player content");
        assert!(!state.include_public_keys);
    }
}
