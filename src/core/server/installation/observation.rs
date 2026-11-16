use serde::Serialize;

use super::{ExecutableVersion, ServerInstallation, manifest::parse_manifest};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct InstallationBuildObservation {
    pub executable_version: Option<ExecutableVersion>,
    pub executable_version_source: Option<&'static str>,
    pub steam_manifest: Option<SteamBuildObservation>,
    pub tested_engine_layout: Option<super::super::engine_layout::TestedEngineLayout>,
    pub vendor_authenticity_verified: bool,
    pub package_completeness_verified: bool,
    pub engine_layout_verified: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SteamBuildObservation {
    pub app_id: u32,
    pub build_id: String,
    pub branch: Option<String>,
    pub branch_conflict: bool,
}

pub(in crate::core::server) fn observe_build(
    installation: &ServerInstallation,
) -> InstallationBuildObservation {
    let executable_version =
        super::version::observe_version(&installation.directory.join(&installation.executable));
    InstallationBuildObservation {
        executable_version,
        executable_version_source: executable_version.map(|_| "windows_product_version_resource"),
        steam_manifest: observe_steam_manifest(installation),
        tested_engine_layout: super::super::engine_layout::observe_tested_layout(installation),
        vendor_authenticity_verified: false,
        package_completeness_verified: false,
        engine_layout_verified: false,
    }
}

fn observe_steam_manifest(installation: &ServerInstallation) -> Option<SteamBuildObservation> {
    if installation.game != super::ServerDefinitionKind::Arma3 {
        return None;
    }
    let common = installation.directory.parent()?;
    let steamapps = common.parent()?;
    if !common.file_name()?.eq_ignore_ascii_case("common")
        || !steamapps.file_name()?.eq_ignore_ascii_case("steamapps")
    {
        return None;
    }
    let mut observations = vec![];
    for app_id in [233780, 107410] {
        let path = steamapps.join(format!("appmanifest_{app_id}.acf"));
        if super::super::store::check_regular_file(&path).is_err() {
            continue;
        }
        let Ok(bytes) = super::super::read_bounded(&path) else {
            continue;
        };
        let Some(manifest) = parse_manifest(&bytes, app_id) else {
            continue;
        };
        let Ok(observed) = common.join(&manifest.install_dir).canonicalize() else {
            continue;
        };
        if observed != installation.directory {
            continue;
        }
        observations.push(SteamBuildObservation {
            app_id: manifest.app_id,
            build_id: manifest.build_id,
            branch: manifest.branch,
            branch_conflict: manifest.branch_conflict,
        });
    }
    (observations.len() == 1).then(|| observations.remove(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_example_is_an_observation_without_runtime_authority() {
        let observation = InstallationBuildObservation {
            executable_version: Some(ExecutableVersion {
                major: 2,
                minor: 22,
                patch: 0,
                build: 150000,
            }),
            executable_version_source: Some("windows_product_version_resource"),
            steam_manifest: Some(SteamBuildObservation {
                app_id: 233780,
                build_id: "246810".into(),
                branch: Some("public".into()),
                branch_conflict: false,
            }),
            ..Default::default()
        };
        let example: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../examples/json/server_backend/server_installation_build_observation.json"
        ))
        .unwrap();
        assert_eq!(serde_json::to_value(observation).unwrap(), example);
    }
}
