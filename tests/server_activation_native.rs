#![cfg(all(target_arch = "x86_64", any(windows, target_os = "linux")))]

#[path = "server_activation_native/acceptance.rs"]
mod acceptance;
#[cfg(target_os = "linux")]
#[path = "server_activation_native/console.rs"]
mod console;
#[path = "server_activation_native/engine.rs"]
mod engine;
#[path = "server_activation_native/inventory.rs"]
mod inventory;
#[path = "server_engine_native/mission.rs"]
mod mission;
#[path = "server_engine_native/proof.rs"]
mod proof;

#[test]
#[ignore = "Requires an existing accepted Arma native package and private scratch root"]
fn native_activation_consumes_and_rolls_back_reviewed_generations() -> std::io::Result<()> {
    acceptance::run()
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "Contained console fixture; invoked only by the native acceptance parent"]
fn native_activation_console_fixture() -> std::io::Result<()> {
    console::run()
}
