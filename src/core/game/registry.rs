use std::sync::OnceLock;

use super::GameModule;
use super::arma3::Arma3Module;
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

    /// All registered game modules, in registration order. Feeds the game
    /// space creation UI/CLI.
    pub fn available(&self) -> impl Iterator<Item = &dyn GameModule> {
        self.modules.iter().map(|module| module.as_ref())
    }

    /// The module of the active game space, or `None` when the space names a
    /// game this build does not register (hand-edited `games.json`, or a
    /// downgrade from a build that had more modules).
    pub fn active_module(&self) -> Option<&dyn GameModule> {
        self.get(&spaces::active_game_space().game_id)
    }

    /// The module of the active game space, falling back to the default module
    /// when the space names an unregistered game.
    ///
    /// The fallback exists so read-only callers (labels, capability queries)
    /// keep working, but it is deliberately loud: silently treating an unknown
    /// space as Arma 3 would let auto-detection write an Arma path into it and
    /// let an Arma-shaped launch plan run against a foreign install. Callers
    /// that act on the module should use [`Self::active_module`] and refuse the
    /// operation when it is `None`.
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
