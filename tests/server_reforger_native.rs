#![cfg(all(target_arch = "x86_64", any(windows, target_os = "linux")))]

#[path = "server_reforger_native/discovery.rs"]
mod discovery;

#[path = "server_reforger_native/layout.rs"]
mod layout;

#[path = "server_reforger_native/lifecycle.rs"]
mod lifecycle;

#[test]
#[ignore = "Requires a read-only Reforger source and private native scenario-end scratch"]
fn native_reforger_scenario_end_and_stop_wins() -> std::io::Result<()> {
    layout::run_scenario_end()
}

#[test]
#[ignore = "Requires a read-only Reforger source, verified private payload and native RCon scratch"]
fn native_reforger_rcon_permissions_and_filters() -> std::io::Result<()> {
    layout::run_rcon_permissions()
}

#[test]
#[ignore = "Requires an explicit read-only Reforger dedicated source and private scratch directory"]
fn native_reforger_scenario_discovery() -> std::io::Result<()> {
    discovery::run()
}

#[test]
#[ignore = "Requires a read-only Reforger dedicated source and private native layout scratch"]
fn native_reforger_layout_isolation() -> std::io::Result<()> {
    layout::run()
}

#[test]
#[ignore = "Requires an accepted Reforger dedicated source and private foreground lifecycle scratch"]
fn native_reforger_retained_owner_lifecycle() -> std::io::Result<()> {
    lifecycle::run()
}

#[test]
#[ignore = "Requires a read-only Reforger source, private scratch and an explicit Workshop addon GUID"]
fn native_reforger_addon_acquisition() -> std::io::Result<()> {
    layout::run_addons()
}

#[test]
#[ignore = "Requires native Reforger source, private scratch and explicit addon/dependency GUIDs"]
fn native_reforger_addon_retained_lifecycle() -> std::io::Result<()> {
    lifecycle::run_addons()
}
