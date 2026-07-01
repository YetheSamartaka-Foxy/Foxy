use super::app::agent_driver::AgentGuiLaunchConfig;

pub(crate) fn main(debug_mode: bool, agent_gui: AgentGuiLaunchConfig) {
    super::launcher::main(debug_mode, agent_gui);
}
