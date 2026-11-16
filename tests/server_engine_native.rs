#![cfg(all(target_arch = "x86_64", any(windows, target_os = "linux")))]

#[path = "server_engine_native/mod.rs"]
mod native;

#[test]
#[ignore = "Requires explicit real Arma source and private scratch directories"]
fn native_arma_layout_observation() -> std::io::Result<()> {
    native::observe()
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "Contained console child fixture; invoked only by the explicit native parent"]
fn native_engine_console_fixture() -> std::io::Result<()> {
    native::console::run()
}

#[cfg(windows)]
#[test]
#[ignore = "Requires an installed regular Arma client, Steam and private signing tools"]
fn native_arma_key_acceptance() -> std::io::Result<()> {
    native::keys::observe()
}
