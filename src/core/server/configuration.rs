use serde::{Deserialize, Serialize};

use super::{ServerDefinition, ServerInstance, ServerInstanceId, ServerStore, ServerStoreError};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigurationFile {
    pub relative_path: String,
    pub contents: String,
    pub blake3: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigurationPreview {
    pub format_version: u32,
    pub instance_id: ServerInstanceId,
    pub instance_revision: u64,
    pub fingerprint: String,
    pub files: Vec<ConfigurationFile>,
    pub warnings: Vec<String>,
    pub launch_ready: bool,
}

impl ServerStore {
    pub fn configuration_preview(
        &self,
        id: &ServerInstanceId,
    ) -> Result<ConfigurationPreview, ServerStoreError> {
        if !self.capabilities().configuration_preview {
            return Err(ServerStoreError::UnsupportedGame);
        }
        render(&self.show(id)?)
    }
}

pub(super) fn render(instance: &ServerInstance) -> Result<ConfigurationPreview, ServerStoreError> {
    instance
        .validate(&instance.game_space_id, instance.definition.kind())
        .map_err(ServerStoreError::Invalid)?;
    let mut files = Vec::new();
    let mut warnings = vec!["Configuration preview only. Engine compatibility, content, credentials and launch readiness remain unverified.".into()];
    match &instance.definition {
        ServerDefinition::Arma3(settings) => {
            if settings.auto_init
                && settings
                    .mission_cycle
                    .iter()
                    .any(|mission| mission.enabled && !mission.params.is_empty())
            {
                warnings.push("Auto-init with custom mission Params depends on the engine and mission. The accepted native runtime has focused parameter evidence; verify other mission behavior and defaults before use.".into());
            }
            let signatures = match settings.verify_signatures {
                super::SignatureVerification::Disabled => 0,
                super::SignatureVerification::V2 => 2,
            };
            let enabled_headless = settings
                .headless_clients
                .iter()
                .filter(|client| client.enabled)
                .count();
            let derived_admission =
                enabled_headless > 0 && settings.server.headless_clients.is_none();
            let server_settings = if derived_admission {
                let mut server = (*settings.server).clone();
                server.headless_clients = Some(vec![super::HEADLESS_LOOPBACK.into()]);
                server.render()
            } else {
                settings.server.render()
            }
            .map_err(ServerStoreError::Invalid)?;
            if !settings.headless_clients.is_empty() {
                warnings.push(format!("{} of {} headless clients are enabled. They connect from {} only after an explicit headless start once the server answers queries. Mission slots and AI offload are not verified.", enabled_headless, settings.headless_clients.len(), super::HEADLESS_LOOPBACK));
            }
            if derived_admission {
                warnings.push(format!("headlessClients[] is unset, so Foxy admits {} for the enabled headless clients.", super::HEADLESS_LOOPBACK));
            }
            let mut contents = format!(
                "hostname = {};\nmaxPlayers = {};\npersistent = {};\nBattlEye = {};\nverifySignatures = {};\n{}class Missions\n{{\n",
                super::arma3_config::quoted(&settings.hostname),
                settings.max_players,
                u8::from(settings.persistent),
                u8::from(settings.battleye),
                signatures,
                server_settings
            );
            for (index, mission) in settings
                .mission_cycle
                .iter()
                .filter(|mission| mission.enabled)
                .enumerate()
            {
                let difficulty = match mission.difficulty {
                    super::Arma3Difficulty::Recruit => "Recruit",
                    super::Arma3Difficulty::Regular => "Regular",
                    super::Arma3Difficulty::Veteran => "Veteran",
                    super::Arma3Difficulty::Custom => "Custom",
                };
                contents.push_str(&format!(
                    "    class Mission{index}\n    {{\n        template = {};\n        difficulty = \"{difficulty}\";\n",
                    super::arma3_config::quoted(&mission.template)
                ));
                if !mission.params.is_empty() {
                    contents.push_str("        class Params\n        {\n");
                    for (key, value) in &mission.params {
                        contents.push_str(&format!(
                            "            {key} = {};\n",
                            super::arma3_config::number_text(value)
                        ));
                    }
                    contents.push_str("        };\n");
                }
                contents.push_str("    };\n");
            }
            contents.push_str("};\n");
            let basic = settings.basic.render().map_err(ServerStoreError::Invalid)?;
            let names = &settings.configuration;
            files.push(file(names.server_file(), contents));
            files.push(file(names.basic_file(), basic));
            if let Some(profile) = &settings.difficulty {
                files.push(file(
                    &format!("{}.Arma3Profile", names.profile_name()),
                    profile.render().map_err(ServerStoreError::Invalid)?,
                ));
            }
            let mut intent = serde_json::Map::new();
            intent.insert("game_port".into(), settings.game_port.into());
            intent.insert("auto_init".into(), settings.auto_init.into());
            if let Some(server) = &names.server {
                intent.insert("server_config".into(), server.as_str().into());
            }
            if let Some(basic) = &names.basic {
                intent.insert("basic_config".into(), basic.as_str().into());
            }
            if let Some(profile) = &names.profile_name {
                intent.insert("profile_name".into(), profile.as_str().into());
            }
            files.push(file(
                "launch-intent.json",
                serde_json::to_string_pretty(&serde_json::Value::Object(intent))
                    .map_err(|_| ServerStoreError::Corrupt)?,
            ));
            if settings.server.has_credentials() {
                warnings.push("Credential references are redacted in this preview; activation requires protected credential resolution.".into());
            }
            let added_settings = !settings.server.is_empty()
                || settings.difficulty.is_some()
                || !settings.configuration.is_empty()
                || settings
                    .mission_cycle
                    .iter()
                    .any(|mission| mission.enabled && !mission.params.is_empty());
            if !added_settings {
                if settings.basic.is_empty() {
                    warnings.push("Arma port and auto-init are launch intent, not server.cfg fields. Basic tuning is empty and custom difficulty/profile settings are not generated.".into());
                } else {
                    warnings.push("Arma port and auto-init are launch intent, not server.cfg fields. Custom difficulty profiles are not generated.".into());
                }
            } else {
                warnings.push(
                    "Arma port and auto-init are launch intent, not server.cfg fields.".into(),
                );
            }
            let uses_custom = settings.server.forced_difficulty
                == Some(super::Arma3Difficulty::Custom)
                || settings.mission_cycle.iter().any(|mission| {
                    mission.enabled && mission.difficulty == super::Arma3Difficulty::Custom
                });
            match (&settings.difficulty, uses_custom) {
                (None, true) => warnings.push("Custom difficulty has no managed profile. The engine uses whatever its existing server profile contains.".into()),
                (Some(_), false) => warnings.push("The managed difficulty profile applies only to missions or a forced difficulty set to Custom.".into()),
                _ => {}
            }
            if !settings.mission_cycle.iter().any(|mission| mission.enabled) {
                warnings.push("No mission cycle is configured.".into());
            }
        }
        ServerDefinition::Reforger(settings) => {
            let value = super::reforger_schema::render_server_json(settings);
            warnings.extend(super::reforger_schema::preview_warnings(settings));
            files.push(file(
                "server.json",
                serde_json::to_string_pretty(&value).map_err(|_| ServerStoreError::Corrupt)?,
            ));
        }
    }
    super::config_import::render_imported(instance, &mut files, &mut warnings)?;
    if let ServerDefinition::Arma3(settings) = &instance.definition
        && settings
            .headless_clients
            .iter()
            .any(|client| client.enabled)
        && instance
            .configuration_import
            .as_ref()
            .is_some_and(|imported| {
                imported
                    .files
                    .iter()
                    .any(|file| file.role == super::ConfigurationRole::Server)
            })
    {
        warnings.push(format!("The operator-owned server configuration is not rewritten. It must admit {} in headlessClients[] for local headless clients.", super::HEADLESS_LOOPBACK));
    }
    if files.iter().map(|file| file.contents.len()).sum::<usize>() > 1024 * 1024 {
        return Err(ServerStoreError::Invalid(
            "Rendered configuration exceeds 1 MiB".into(),
        ));
    }
    let fingerprint = blake3::hash(
        &serde_json::to_vec(&(1u32, instance, &files)).map_err(|_| ServerStoreError::Corrupt)?,
    )
    .to_hex()
    .to_string();
    Ok(ConfigurationPreview {
        format_version: 1,
        instance_id: instance.id.clone(),
        instance_revision: instance.revision,
        fingerprint,
        files,
        warnings,
        launch_ready: false,
    })
}

fn file(relative_path: &str, contents: String) -> ConfigurationFile {
    ConfigurationFile {
        relative_path: relative_path.into(),
        blake3: blake3::hash(contents.as_bytes()).to_hex().to_string(),
        contents,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::server::{
        Arma3Difficulty, Arma3DifficultyOptions, Arma3DifficultyProfile, Arma3Mission,
        ReforgerAddonRequirement, ServerDefinitionKind,
    };
    use serde_json::{Number, json};

    #[test]
    fn arma_configuration_separates_launch_intent_and_quotes_hostname_as_data() {
        let mut instance =
            ServerInstance::draft("alpha", ServerDefinitionKind::Arma3, "MainServer").unwrap();
        let ServerDefinition::Arma3(settings) = &mut instance.definition else {
            unreachable!()
        };
        settings.mission_cycle.push(Arma3Mission {
            template: "Example.World".into(),
            difficulty: Arma3Difficulty::Veteran,
            enabled: true,
            source: None,
            params: Default::default(),
        });
        let preview = render(&instance).unwrap();
        assert_eq!(preview, render(&instance).unwrap());
        assert!(!preview.launch_ready);
        assert!(
            preview.files[0]
                .contents
                .contains("template = \"Example.World\";")
        );
        assert!(preview.files[0].contents.contains("verifySignatures = 2;"));
        assert!(!preview.files[0].contents.contains("game_port"));
        assert!(preview.files[1].contents.is_empty());
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&preview.files[2].contents).unwrap()["game_port"],
            2302
        );
        let ServerDefinition::Arma3(settings) = &mut instance.definition else {
            unreachable!()
        };
        settings.hostname = "quoted\"; unexpected=1;".into();
        let escaped = render(&instance).unwrap();
        assert!(
            escaped.files[0]
                .contents
                .contains("hostname = \"quoted\"\"; unexpected=1;\";")
        );
    }

    #[test]
    fn basic_cfg_renders_set_values_and_stays_empty_by_default() {
        let mut instance =
            ServerInstance::draft("alpha", ServerDefinitionKind::Arma3, "MainServer").unwrap();
        {
            let ServerDefinition::Arma3(settings) = &mut instance.definition else {
                unreachable!()
            };
            settings.basic.min_bandwidth = Some(128_000);
            settings.basic.max_bandwidth = Some(256_000);
        }
        let preview = render(&instance).unwrap();
        let basic = preview
            .files
            .iter()
            .find(|file| file.relative_path == "basic.cfg")
            .unwrap();
        assert!(basic.contents.contains("MinBandwidth = 128000;"));
        assert!(basic.contents.contains("MaxBandwidth = 256000;"));
        {
            let ServerDefinition::Arma3(settings) = &mut instance.definition else {
                unreachable!()
            };
            settings.basic.min_bandwidth = Some(0);
        }
        assert!(render(&instance).is_err());
    }

    #[test]
    fn legacy_arma_preview_keeps_its_files_warning_and_fingerprint_contract() {
        let example: serde_json::Value = serde_json::from_str(include_str!(
            "../../../examples/json/server_backend/arma3_configuration_revision.json"
        ))
        .unwrap();
        let instance: ServerInstance = serde_json::from_value(example["instance"].clone()).unwrap();
        let preview = render(&instance).unwrap();
        assert_eq!(
            preview.files[0].contents,
            "hostname = \"MainServer\";\nmaxPlayers = 64;\npersistent = 0;\nBattlEye = 1;\nverifySignatures = 2;\nclass Missions\n{\n};\n"
        );
        assert_eq!(preview.files[1].contents, "");
        assert_eq!(
            preview.files[2].contents,
            "{\n  \"game_port\": 2302,\n  \"auto_init\": false\n}"
        );
        assert_eq!(
            preview.warnings[1],
            "Arma port and auto-init are launch intent, not server.cfg fields. Basic tuning is empty and custom difficulty/profile settings are not generated."
        );
        assert_eq!(preview.fingerprint, render(&instance).unwrap().fingerprint);
        let example: serde_json::Value = serde_json::from_str(include_str!(
            "../../../examples/json/server_backend/arma3_configuration_revision.json"
        ))
        .unwrap();
        assert_eq!(preview.fingerprint, example["preview"]["fingerprint"]);
        assert_eq!(
            preview.files.len(),
            example["preview"]["files"].as_array().unwrap().len()
        );
        for (actual, expected) in preview
            .files
            .iter()
            .zip(example["preview"]["files"].as_array().unwrap())
        {
            assert_eq!(actual.relative_path, expected["relative_path"]);
            assert_eq!(actual.contents, expected["contents"]);
            assert_eq!(actual.blake3, expected["blake3"]);
        }
    }

    #[test]
    fn arma_renderer_emits_nested_fields_params_profile_and_names() {
        let mut instance =
            ServerInstance::draft("alpha", ServerDefinitionKind::Arma3, "MainServer").unwrap();
        let ServerDefinition::Arma3(settings) = &mut instance.definition else {
            unreachable!()
        };
        settings.hostname = "Quoted \"host\" \\ path".into();
        settings.configuration.server = Some("custom-server.cfg".into());
        settings.configuration.basic = Some("custom-basic.cfg".into());
        settings.configuration.profile_name = Some("PrivateProfile".into());
        settings.basic.min_error_to_send = Some(Number::from_f64(0.125).unwrap());
        settings.server.motd = Some(vec![]);
        settings.server.anti_flood = Some(Box::new(crate::core::server::Arma3AntiFlood {
            cycle_limit: Some(10),
            ..Default::default()
        }));
        settings.difficulty = Some(Box::new(Arma3DifficultyProfile {
            options: Arma3DifficultyOptions {
                reduced_damage: Some(false),
                ..Default::default()
            },
            ai_level_preset: Some(3),
            custom_ai: Default::default(),
        }));
        let mut params = std::collections::BTreeMap::new();
        params.insert("mode".into(), Number::from_f64(0.75).unwrap());
        settings.mission_cycle.push(Arma3Mission {
            template: "Example.World".into(),
            difficulty: Arma3Difficulty::Custom,
            enabled: true,
            source: None,
            params,
        });

        let preview = render(&instance).unwrap();
        let server = preview
            .files
            .iter()
            .find(|f| f.relative_path == "custom-server.cfg")
            .unwrap();
        assert!(
            server
                .contents
                .contains("hostname = \"Quoted \"\"host\"\" \\ path\";")
        );
        assert!(server.contents.contains("motd[] = {};"));
        assert!(
            server
                .contents
                .contains("class AntiFlood\n{\ncycleLimit = 10;\n};")
        );
        assert!(
            server
                .contents
                .contains("class Params\n        {\n            mode = 0.75;")
        );
        let basic = preview
            .files
            .iter()
            .find(|f| f.relative_path == "custom-basic.cfg")
            .unwrap();
        assert_eq!(basic.contents, "MinErrorToSend = 0.125;\n");
        let profile = preview
            .files
            .iter()
            .find(|f| f.relative_path == "PrivateProfile.Arma3Profile")
            .unwrap();
        assert!(profile.contents.contains("reducedDamage = 0;"));
        assert!(profile.contents.contains("aiLevelPreset = 3;"));
        let intent: serde_json::Value =
            serde_json::from_str(&preview.files.last().unwrap().contents).unwrap();
        assert_eq!(intent["server_config"], "custom-server.cfg");
        assert_eq!(intent["basic_config"], "custom-basic.cfg");
        assert_eq!(intent["profile_name"], "PrivateProfile");
    }

    #[test]
    fn custom_names_without_profile_and_secret_refs_are_safe_in_preview() {
        let mut instance =
            ServerInstance::draft("alpha", ServerDefinitionKind::Arma3, "MainServer").unwrap();
        let ServerDefinition::Arma3(settings) = &mut instance.definition else {
            unreachable!()
        };
        settings.server.password_ref = Some("secret_00000000000000000000000000000000".into());
        settings.configuration.server = Some("named.cfg".into());
        let preview = render(&instance).unwrap();
        assert!(
            preview
                .files
                .iter()
                .all(|f| f.relative_path != "FoxyServer.Arma3Profile")
        );
        assert!(preview.warnings.iter().any(|w| w.contains("redacted")));
        let server = &preview.files[0].contents;
        assert!(server.contains("password = \"<redacted secret reference>\";"));
        assert!(!server.contains("secret_00000000000000000000000000000000"));
        let intent: serde_json::Value =
            serde_json::from_str(&preview.files.last().unwrap().contents).unwrap();
        assert_eq!(intent["server_config"], "named.cfg");
        assert!(intent.get("basic_config").is_none());
        assert!(intent.get("profile_name").is_none());
    }

    #[test]
    fn complete_managed_example_matches_independent_engine_files() {
        let instance: ServerInstance = serde_json::from_str(include_str!(
            "../../../examples/json/server_backend/arma3_managed_configuration.json"
        ))
        .unwrap();
        let preview = render(&instance).unwrap();
        for (name, expected) in [
            (
                "dedicated.cfg",
                include_str!("../../../examples/arma3/dedicated.cfg"),
            ),
            (
                "network.cfg",
                include_str!("../../../examples/arma3/network.cfg"),
            ),
            (
                "Managed.Arma3Profile",
                include_str!("../../../examples/arma3/Managed.Arma3Profile"),
            ),
        ] {
            let file = preview
                .files
                .iter()
                .find(|f| f.relative_path == name)
                .unwrap();
            assert_eq!(file.contents, expected.replace("\r\n", "\n"), "{name}");
        }
    }

    #[test]
    fn default_named_custom_profile_keeps_legacy_intent_and_warns_about_custom_use() {
        let mut instance = ServerInstance::draft(
            "alpha",
            ServerDefinitionKind::Arma3,
            "class Missions \"Host\"",
        )
        .unwrap();
        let ServerDefinition::Arma3(settings) = &mut instance.definition else {
            unreachable!()
        };
        settings.difficulty = Some(Box::default());
        settings.server.disable_von = Some(true);
        let preview = render(&instance).unwrap();
        assert!(
            preview.files[0]
                .contents
                .starts_with("hostname = \"class Missions \"\"Host\"\"\";\n")
        );
        assert!(
            preview.files[0]
                .contents
                .contains("disableVoN = 1;\nclass Missions\n")
        );
        assert!(
            preview
                .files
                .iter()
                .any(|file| file.relative_path == "FoxyServer.Arma3Profile")
        );
        let intent: serde_json::Value =
            serde_json::from_str(&preview.files.last().unwrap().contents).unwrap();
        assert_eq!(intent, json!({"game_port": 2302, "auto_init": false}));
        assert!(
            preview
                .warnings
                .iter()
                .any(|warning| warning.contains("applies only to missions"))
        );

        let ServerDefinition::Arma3(settings) = &mut instance.definition else {
            unreachable!()
        };
        settings.difficulty = None;
        settings.mission_cycle.push(Arma3Mission {
            enabled: true,
            source: None,
            template: "Example.World".into(),
            difficulty: Arma3Difficulty::Custom,
            params: Default::default(),
        });
        let unmanaged = render(&instance).unwrap();
        assert!(
            unmanaged
                .warnings
                .iter()
                .any(|warning| warning.contains("no managed profile"))
        );
    }

    #[test]
    fn reforger_json_escapes_names_and_preserves_unset_false_true_addon_policy() {
        let mut instance =
            ServerInstance::draft("beta", ServerDefinitionKind::Reforger, "MainServer").unwrap();
        let ServerDefinition::Reforger(settings) = &mut instance.definition else {
            unreachable!()
        };
        settings.name = "Quoted \"name\" \\ folder".into();
        settings.addons = [None, Some(false), Some(true)]
            .into_iter()
            .enumerate()
            .map(|(index, required)| ReforgerAddonRequirement {
                addon_id: format!("{index:016X}"),
                version: None,
                required,
                preserved: Default::default(),
            })
            .collect();
        let preview = render(&instance).unwrap();
        let value: serde_json::Value = serde_json::from_str(&preview.files[0].contents).unwrap();
        assert_eq!(value["game"]["name"], "Quoted \"name\" \\ folder");
        assert!(value["game"]["mods"][0].get("required").is_none());
        assert_eq!(value["game"]["mods"][1]["required"], false);
        assert_eq!(value["game"]["mods"][2]["required"], true);
        assert!(value.get("a2s").is_none());
        assert!(value.get("bindAddress").is_none());
        assert!(value["game"].get("scenarioId").is_none());

        let ServerDefinition::Reforger(settings) = &mut instance.definition else {
            unreachable!()
        };
        settings.bind_address = Some(std::net::Ipv4Addr::LOCALHOST);
        settings.a2s_port = Some(27016);
        let bound_preview = render(&instance).unwrap();
        let bound: serde_json::Value =
            serde_json::from_str(&bound_preview.files[0].contents).unwrap();
        assert_eq!(bound["bindAddress"], "127.0.0.1");
        assert_eq!(bound["a2s"]["port"], 27016);
        assert_eq!(bound["a2s"]["address"], "127.0.0.1");
        let old = preview.fingerprint;
        instance.revision += 1;
        assert_ne!(old, render(&instance).unwrap().fingerprint);
    }
}
