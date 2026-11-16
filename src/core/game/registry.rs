use std::sync::OnceLock;

use super::GameModule;
use super::arma3::Arma3Module;
use super::generic_game::GenericGameModule;
use super::reforger::ReforgerModule;
use super::spaces;
use super::twwh3::TotalWarWarhammer3Module;

pub struct GameRegistry {
    modules: Vec<Box<dyn GameModule>>,
}

impl GameRegistry {
    fn new() -> Self {
        let modules: Vec<Box<dyn GameModule>> = vec![
            Box::new(Arma3Module),
            Box::new(TotalWarWarhammer3Module),
            Box::new(ReforgerModule),
            Box::new(GenericGameModule),
        ];
        for module in &modules {
            log::info!(
                "Registered game module {} ({}) with capabilities [{}]",
                module.display_name(),
                module.id(),
                module.capabilities().summary()
            );
        }
        Self { modules }
    }

    pub fn get(&self, game_id: &str) -> Option<&dyn GameModule> {
        self.modules
            .iter()
            .find(|module| module.id() == game_id)
            .map(|module| module.as_ref())
    }

    pub fn available(&self) -> impl Iterator<Item = &dyn GameModule> {
        self.modules.iter().map(|module| module.as_ref())
    }

    /// Hand-edited or downgraded registries may name an unregistered game; keep it unknown instead of silently substituting.
    pub fn active_module(&self) -> Option<&dyn GameModule> {
        self.get(&spaces::active_game_space().game_id)
    }

    /// Return the active module or the default for read-only callers. Operations
    /// must use [`Self::active_module`] and refuse unknown game IDs.
    pub fn active(&self) -> &dyn GameModule {
        let active = spaces::active_game_space();
        match self.get(&active.game_id) {
            Some(module) => module,
            None => {
                log::warn!(
                    "Game space {} names unregistered game {}; falling back to the default module for read-only use",
                    active.space_id,
                    active.game_id
                );
                self.get(spaces::DEFAULT_GAME_SPACE_ID)
                    .expect("default game module must be registered")
            }
        }
    }
}

pub fn registry() -> &'static GameRegistry {
    static REGISTRY: OnceLock<GameRegistry> = OnceLock::new();
    REGISTRY.get_or_init(GameRegistry::new)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::game::DedicatedServerSupport;

    #[test]
    fn dedicated_server_support_is_separate_from_client_features() {
        for game in ["arma3", "reforger"] {
            assert_eq!(
                registry().get(game).unwrap().dedicated_server_support(),
                DedicatedServerSupport::WorkInProgress
            );
        }
        for game in ["generic", "twwh3"] {
            assert_eq!(
                registry().get(game).unwrap().dedicated_server_support(),
                DedicatedServerSupport::Unsupported
            );
        }
        assert!(registry().get("unknown").is_none());
        assert!(
            !registry()
                .get("reforger")
                .unwrap()
                .capabilities()
                .steam_workshop
        );
    }
}
