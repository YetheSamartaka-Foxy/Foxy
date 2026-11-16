#![recursion_limit = "256"]
#![cfg(all(target_arch = "x86_64", any(windows, target_os = "linux")))]

#[path = "server_configuration_native/acceptance.rs"]
mod acceptance;
#[path = "server_configuration_native/mission.rs"]
mod mission;

#[test]
#[ignore = "Requires an accepted native Arma package and private configuration scratch"]
fn native_arma_managed_configuration_generations() -> std::io::Result<()> {
    acceptance::run()
}

#[test]
#[ignore = "Requires an accepted native Arma package and private operator-import scratch"]
fn native_arma_operator_import_configuration() -> std::io::Result<()> {
    acceptance::run_imported()
}

#[test]
#[ignore = "Requires an accepted native Arma package and private mission-library scratch"]
fn native_arma_mission_library_and_ordered_cycle() -> std::io::Result<()> {
    acceptance::run_mission_library()
}

#[test]
#[ignore = "Requires an accepted native Arma package, signed public-key fixtures and private key-trust scratch"]
fn native_arma_reviewed_key_trust_and_addon_roles() -> std::io::Result<()> {
    acceptance::run_key_trust()
}

#[test]
#[ignore = "Requires an accepted native Arma package, signed public-key fixtures and private reconciliation scratch"]
fn native_arma_shared_consumers_pins_and_reconciliation() -> std::io::Result<()> {
    acceptance::run_reconciliation()
}
