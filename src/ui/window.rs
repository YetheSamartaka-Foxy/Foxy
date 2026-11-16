use super::app::agent_driver::AgentGuiLaunchConfig;
use super::app::debug_modals::DebugModal;

pub(crate) fn main(
    debug_mode: bool,
    agent_gui: AgentGuiLaunchConfig,
    debug_modals: Vec<DebugModal>,
    mode_override: Option<crate::core::app_mode::AppMode>,
) {
    super::launcher::main(debug_mode, agent_gui, debug_modals, mode_override);
}
