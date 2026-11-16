#![cfg_attr(not(test), allow(dead_code))]

use super::ServerDefinitionKind;

mod accepted;
pub use accepted::TestedEngineLayout;

pub(super) fn observe_tested_layout(
    installation: &super::ServerInstallation,
) -> Option<TestedEngineLayout> {
    accepted::observe(installation)
}

pub(crate) fn layout_blocker(kind: ServerDefinitionKind) -> (&'static str, &'static str) {
    match kind {
        ServerDefinitionKind::Arma3 => (
            "arma_layout_unverified",
            "Arma mission/key directory flags and profile resolution require verified engine support. Proposed paths are not launch arguments; an adopted installation grants no permission to create mission directories inside it.",
        ),
        ServerDefinitionKind::Reforger => (
            "reforger_layout_unverified",
            "Native profile, addon, temporary and save destinations require engine validation.",
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_executable_name_does_not_make_the_layout_supported() {
        let root = tempfile::tempdir().unwrap();
        let exe = root.path().join("arma3server_x64.exe");
        std::fs::write(&exe, b"not an engine").unwrap();
        assert!(
            super::super::ServerInstallation::inspect(root.path(), ServerDefinitionKind::Arma3)
                .is_err()
        );
        assert_eq!(
            layout_blocker(ServerDefinitionKind::Arma3).0,
            "arma_layout_unverified"
        );
        assert_eq!(
            layout_blocker(ServerDefinitionKind::Reforger).0,
            "reforger_layout_unverified"
        );
        assert_eq!(std::fs::read(&exe).unwrap(), b"not an engine");
    }
}
