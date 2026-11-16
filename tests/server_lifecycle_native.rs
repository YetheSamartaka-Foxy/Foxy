#![cfg(all(target_arch = "x86_64", any(windows, target_os = "linux")))]

#[path = "server_lifecycle_native/acceptance.rs"]
mod acceptance;
#[path = "server_engine_native/mission.rs"]
mod mission;

#[test]
#[ignore = "Requires two accepted Arma runtime generations and private lifecycle scratch"]
fn native_arma_retained_owner_lifecycle() -> std::io::Result<()> {
    acceptance::run()
}

#[test]
#[ignore = "Requires an accepted Arma runtime and private headless scratch"]
fn native_arma_headless_clients() -> std::io::Result<()> {
    acceptance::headless::run()
}

#[test]
#[ignore = "Requires an accepted Arma runtime and private headless lifecycle scratch"]
fn native_arma_headless_lifecycle() -> std::io::Result<()> {
    acceptance::headless_lifecycle::run()
}
