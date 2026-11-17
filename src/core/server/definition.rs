use super::arma3_config::{
    Arma3BasicSettings, Arma3ConfigurationNames, Arma3DifficultyProfile, Arma3ServerSettings,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::net::Ipv4Addr;

pub const SERVER_SCHEMA_VERSION: u32 = 2;

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ServerInstanceId(String);

impl ServerInstanceId {
    pub fn new() -> Self {
        Self(format!("server_{:032x}", rand::random::<u128>()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for ServerInstanceId {
    fn default() -> Self {
        Self::new()
    }
}

impl TryFrom<String> for ServerInstanceId {
    type Error = String;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        let suffix = value.strip_prefix("server_").unwrap_or_default();
        if suffix.len() != 32
            || !suffix
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err("Invalid server instance id".to_string());
        }
        Ok(Self(value))
    }
}

impl From<ServerInstanceId> for String {
    fn from(value: ServerInstanceId) -> Self {
        value.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServerDefinitionKind {
    Arma3,
    Reforger,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "game",
    content = "settings",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum ServerDefinition {
    Arma3(Arma3ServerDefinition),
    Reforger(Box<ReforgerServerDefinition>),
}

impl ServerDefinition {
    pub fn draft(kind: ServerDefinitionKind, name: &str) -> Self {
        match kind {
            ServerDefinitionKind::Arma3 => Self::Arma3(Arma3ServerDefinition {
                hostname: name.to_string(),
                game_port: 2302,
                max_players: 64,
                persistent: false,
                auto_init: false,
                battleye: true,
                verify_signatures: SignatureVerification::V2,
                mission_cycle: Vec::new(),
                basic: Box::default(),
                server: Box::default(),
                difficulty: None,
                configuration: Box::default(),
                key_trust: None,
                headless_clients: Vec::new(),
                rcon: None,
            }),
            ServerDefinitionKind::Reforger => Self::Reforger(Box::new(ReforgerServerDefinition {
                name: name.to_string(),
                bind_port: 2001,
                bind_address: None,
                a2s_port: None,
                max_players: 64,
                visible: false,
                scenario_id: None,
                addons: Vec::new(),
                cross_platform: None,
                mods_required_by_default: None,
                game_properties: ReforgerGameProperties::default(),
                preserved: ReforgerPreservedJson::default(),
                redacted_secret_paths: Vec::new(),
                rcon: None,
            })),
        }
    }

    pub fn kind(&self) -> ServerDefinitionKind {
        match self {
            Self::Arma3(_) => ServerDefinitionKind::Arma3,
            Self::Reforger(_) => ServerDefinitionKind::Reforger,
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        match self {
            Self::Arma3(settings) => {
                validate_text(&settings.hostname, "hostname", 256)?;
                if settings.game_port == 0 || settings.max_players == 0 {
                    return Err("Arma 3 game port and player capacity must be nonzero".to_string());
                }
                if settings.auto_init && !settings.persistent {
                    return Err("Arma 3 auto_init requires persistent".to_string());
                }
                for mission in &settings.mission_cycle {
                    if let Some(source) = &mission.source {
                        source.validate()?;
                    }
                    let valid =
                        if matches!(mission.source, Some(super::Arma3MissionSource::AddonClass)) {
                            mission_parameter_name(&mission.template)
                        } else {
                            super::missions::valid_template(&mission.template)
                        };
                    if !valid {
                        return Err("Invalid Arma 3 mission template".to_string());
                    }
                }
                if settings.mission_cycle.len() > 2048 {
                    return Err("Arma 3 mission cycle exceeds 2048 entries".into());
                }
                settings.basic.validate_relationships()?;
                settings.server.validate_relationships()?;
                settings.configuration.validate()?;
                if let Some(trust) = &settings.key_trust {
                    trust.validate()?;
                }
                super::validate_headless_clients(
                    &settings.headless_clients,
                    settings.server.headless_clients.as_deref(),
                )?;
                if let Some(profile) = &settings.difficulty {
                    profile.validate()?;
                }
                if let Some(rcon) = &settings.rcon {
                    rcon.validate(settings.game_port, settings.battleye)?;
                }
                for mission in &settings.mission_cycle {
                    if mission.params.len() > 256 {
                        return Err("Arma 3 missions accept at most 256 parameters".into());
                    }
                    if !mission.params.keys().all(|key| mission_parameter_name(key)) {
                        return Err("Invalid Arma 3 mission parameter identifier".into());
                    }
                }
            }
            Self::Reforger(settings) => {
                validate_text(&settings.name, "server name", 256)?;
                if settings.bind_port == 0
                    || settings.a2s_port == Some(0)
                    || settings.a2s_port == Some(settings.bind_port)
                    || !(1..=128).contains(&settings.max_players)
                {
                    return Err("Invalid Reforger ports or player capacity (1-128)".to_string());
                }
                if let Some(resource) = &settings.scenario_id {
                    validate_resource(resource)?;
                }
                let mut ids = std::collections::HashSet::new();
                for addon in &settings.addons {
                    if !is_guid(&addon.addon_id) || !ids.insert(addon.addon_id.to_ascii_uppercase())
                    {
                        return Err("Invalid or duplicate Reforger addon id".to_string());
                    }
                    if let Some(version) = &addon.version {
                        validate_text(version, "addon version", 128)?;
                    }
                }
                if let Some(rcon) = &settings.rcon {
                    rcon.validate(settings.bind_port, settings.a2s_port)?;
                    if !settings.preserved.rcon.is_empty()
                        && settings.preserved.rcon.keys().any(|key| {
                            matches!(
                                key.as_str(),
                                "address" | "port" | "permission" | "maxClients"
                            )
                        })
                    {
                        return Err(
                            "Preserved Reforger rcon values duplicate typed RCon settings".into(),
                        );
                    }
                }
                super::reforger_schema::validate_reforger(settings)?;
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerInstance {
    pub schema_version: u32,
    pub id: ServerInstanceId,
    pub game_space_id: String,
    pub revision: u64,
    pub display_name: String,
    pub installation_id: Option<super::InstallationId>,
    pub definition: ServerDefinition,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub configuration_import: Option<Box<super::ImportedConfiguration>>,
}

impl ServerInstance {
    pub fn draft(space: &str, kind: ServerDefinitionKind, name: &str) -> Result<Self, String> {
        let instance = Self {
            schema_version: SERVER_SCHEMA_VERSION,
            id: ServerInstanceId::new(),
            game_space_id: space.to_string(),
            revision: 1,
            display_name: name.to_string(),
            installation_id: None,
            definition: ServerDefinition::draft(kind, name),
            configuration_import: None,
        };
        instance.validate(space, kind)?;
        Ok(instance)
    }

    pub fn validate(&self, space: &str, kind: ServerDefinitionKind) -> Result<(), String> {
        if self.schema_version != SERVER_SCHEMA_VERSION {
            return Err("Unsupported server instance schema version".to_string());
        }
        if !crate::core::game::spaces::is_valid_game_space_id(&self.game_space_id)
            || self.game_space_id != space
            || self.definition.kind() != kind
        {
            return Err("Server instance does not match the selected game space".to_string());
        }
        if self.revision == 0 {
            return Err("Server instance revision must be nonzero".to_string());
        }
        validate_text(&self.display_name, "instance name", 128)?;
        self.definition.validate()?;
        if let Some(imported) = &self.configuration_import {
            if kind != ServerDefinitionKind::Arma3 {
                return Err("Operator configuration is currently available for Arma 3 only".into());
            }
            imported.validate()?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Arma3ServerDefinition {
    pub hostname: String,
    pub game_port: u16,
    pub max_players: u16,
    pub persistent: bool,
    pub auto_init: bool,
    pub battleye: bool,
    pub verify_signatures: SignatureVerification,
    pub mission_cycle: Vec<Arma3Mission>,
    #[serde(default, skip_serializing_if = "Arma3BasicSettings::is_empty")]
    pub basic: Box<Arma3BasicSettings>,
    #[serde(default, skip_serializing_if = "Arma3ServerSettings::is_empty")]
    pub server: Box<Arma3ServerSettings>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub difficulty: Option<Box<Arma3DifficultyProfile>>,
    #[serde(default, skip_serializing_if = "Arma3ConfigurationNames::is_empty")]
    pub configuration: Box<Arma3ConfigurationNames>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_trust: Option<Box<super::ServerKeyTrust>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub headless_clients: Vec<super::Arma3HeadlessClient>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rcon: Option<super::Arma3RconSettings>,
}

impl Arma3ServerDefinition {
    pub(crate) fn validate_engine_version(&self, version: u16) -> Result<(), String> {
        super::arma3_config::validate_version(&self.server, Arma3ServerSettings::FIELDS, version)?;
        if let Some(profile) = &self.difficulty {
            profile.validate_version(version)?;
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SignatureVerification {
    Disabled,
    V2,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Arma3Mission {
    #[serde(
        default = "mission_enabled",
        skip_serializing_if = "mission_is_enabled"
    )]
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<super::Arma3MissionSource>,
    pub template: String,
    pub difficulty: Arma3Difficulty,
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub params: std::collections::BTreeMap<String, serde_json::Number>,
}

fn mission_enabled() -> bool {
    true
}
fn mission_is_enabled(enabled: &bool) -> bool {
    *enabled
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Arma3Difficulty {
    Recruit,
    Regular,
    Veteran,
    Custom,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReforgerServerDefinition {
    pub name: String,
    pub bind_port: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bind_address: Option<Ipv4Addr>,
    pub a2s_port: Option<u16>,
    pub max_players: u16,
    pub visible: bool,
    pub scenario_id: Option<String>,
    pub addons: Vec<ReforgerAddonRequirement>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cross_platform: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mods_required_by_default: Option<bool>,
    #[serde(default, skip_serializing_if = "ReforgerGameProperties::is_empty")]
    pub game_properties: ReforgerGameProperties,
    #[serde(default, skip_serializing_if = "ReforgerPreservedJson::is_empty")]
    pub preserved: ReforgerPreservedJson,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub redacted_secret_paths: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rcon: Option<super::ReforgerRconSettings>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReforgerGameProperties {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_max_view_distance: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_min_grass_distance: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network_view_distance: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disable_third_person: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fast_validation: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub battleye: Option<bool>,
}

impl ReforgerGameProperties {
    pub(super) fn is_empty(&self) -> bool {
        self == &Self::default()
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReforgerPreservedJson {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub root: BTreeMap<String, Value>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub a2s: BTreeMap<String, Value>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub game: BTreeMap<String, Value>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub game_properties: BTreeMap<String, Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operating: Option<Value>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub rcon: BTreeMap<String, Value>,
}

impl ReforgerPreservedJson {
    pub(super) fn is_empty(&self) -> bool {
        self == &Self::default()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReforgerAddonRequirement {
    pub addon_id: String,
    pub version: Option<String>,
    pub required: Option<bool>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub preserved: BTreeMap<String, Value>,
}

fn validate_text(value: &str, field: &str, limit: usize) -> Result<(), String> {
    if value.trim().is_empty()
        || value.trim() != value
        || value.chars().count() > limit
        || value.chars().any(char::is_control)
    {
        return Err(format!(
            "Invalid {field}: use 1-{limit} characters without surrounding whitespace or controls"
        ));
    }
    Ok(())
}

fn mission_parameter_name(key: &str) -> bool {
    key.len() <= 64
        && key
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_alphabetic() || byte == b'_')
        && key
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

fn is_guid(value: &str) -> bool {
    value.len() == 16 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

pub(crate) fn scenario_resource_valid(value: &str) -> bool {
    validate_resource(value).is_ok()
}

fn validate_resource(value: &str) -> Result<(), String> {
    let valid = value.starts_with('{')
        && value.as_bytes().get(17) == Some(&b'}')
        && is_guid(value.get(1..17).unwrap_or_default())
        && value.ends_with(".conf")
        && value.len() <= 1024
        && value.get(18..).is_some_and(|path| {
            !path.starts_with('/')
                && !path.contains("..")
                && !path.contains(['\\', ':', '{', '}'])
                && !path.chars().any(char::is_control)
        });
    if valid {
        Ok(())
    } else {
        Err("Scenario must be a full {16-hex GUID}path.conf resource name".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_stable_strict_path_components() {
        let id = ServerInstanceId::new();
        assert_eq!(
            ServerInstanceId::try_from(id.as_str().to_string()).unwrap(),
            id
        );
        for bad in [
            "../server",
            "con",
            "server_1",
            "server_0000000000000000000000000000000/",
        ] {
            assert!(ServerInstanceId::try_from(bad.to_string()).is_err());
        }
    }

    #[test]
    fn both_games_round_trip_without_cross_game_fields_or_defaults() {
        for kind in [ServerDefinitionKind::Arma3, ServerDefinitionKind::Reforger] {
            let instance = ServerInstance::draft("alpha", kind, "MainServer").unwrap();
            let mut value = serde_json::to_value(&instance).unwrap();
            assert_eq!(
                serde_json::from_value::<ServerInstance>(value.clone()).unwrap(),
                instance
            );
            value["definition"]["settings"]["password"] = "private".into();
            assert!(serde_json::from_value::<ServerInstance>(value).is_err());
        }
        let mut value = serde_json::to_value(ServerDefinition::draft(
            ServerDefinitionKind::Arma3,
            "MainServer",
        ))
        .unwrap();
        value["settings"]
            .as_object_mut()
            .unwrap()
            .remove("battleye");
        assert!(serde_json::from_value::<ServerDefinition>(value).is_err());
    }

    #[test]
    fn reforger_bind_address_defaults_to_none_and_is_omitted_from_serialization() {
        let definition = ServerDefinition::draft(ServerDefinitionKind::Reforger, "MainServer");
        let ServerDefinition::Reforger(settings) = &definition else {
            unreachable!()
        };
        assert_eq!(settings.bind_address, None);
        let value = serde_json::to_value(definition).unwrap();
        assert!(value["settings"].get("bind_address").is_none());
    }

    #[test]
    fn validation_rejects_context_versions_controls_and_invalid_game_policy() {
        let mut instance =
            ServerInstance::draft("alpha", ServerDefinitionKind::Arma3, "MainServer").unwrap();
        assert!(
            instance
                .validate("beta", ServerDefinitionKind::Arma3)
                .is_err()
        );
        assert!(
            instance
                .validate("alpha", ServerDefinitionKind::Reforger)
                .is_err()
        );
        instance.schema_version += 1;
        assert!(
            instance
                .validate("alpha", ServerDefinitionKind::Arma3)
                .is_err()
        );
        assert!(
            ServerInstance::draft("alpha", ServerDefinitionKind::Arma3, "\u{1b}Server").is_err()
        );
        instance.schema_version = SERVER_SCHEMA_VERSION;
        let ServerDefinition::Arma3(settings) = &mut instance.definition else {
            unreachable!()
        };
        settings.auto_init = true;
        assert!(instance.definition.validate().is_err());
        for resource in [
            "{0123456789ABCDEF}Missions/Example.conf",
            "{0123456789abcdef}Missions/Example.conf",
        ] {
            assert!(validate_resource(resource).is_ok());
        }
        for resource in [
            "Example.conf",
            "{0123456789ABCDEF}../Example.conf",
            "{0123456789ABCDEF}/Example.conf",
        ] {
            assert!(validate_resource(resource).is_err());
        }
    }

    #[test]
    fn reforger_required_is_tristate_and_duplicate_ids_are_rejected() {
        let mut definition = ServerDefinition::draft(ServerDefinitionKind::Reforger, "MainServer");
        for required in [None, Some(false), Some(true)] {
            let ServerDefinition::Reforger(settings) = &mut definition else {
                unreachable!()
            };
            settings.addons = vec![ReforgerAddonRequirement {
                addon_id: "0123456789ABCDEF".into(),
                version: None,
                required,
                preserved: BTreeMap::new(),
            }];
            assert!(definition.validate().is_ok());
            assert_eq!(
                serde_json::from_str::<ServerDefinition>(
                    &serde_json::to_string(&definition).unwrap()
                )
                .unwrap(),
                definition
            );
        }
        let ServerDefinition::Reforger(settings) = &mut definition else {
            unreachable!()
        };
        settings.addons.push(ReforgerAddonRequirement {
            addon_id: "0123456789abcdef".into(),
            version: None,
            required: None,
            preserved: BTreeMap::new(),
        });
        assert!(definition.validate().is_err());
    }

    #[test]
    fn typed_game_policies_validate_ports_capacity_and_mission_resources() {
        let ServerDefinition::Arma3(mut arma) =
            ServerDefinition::draft(ServerDefinitionKind::Arma3, "MainServer")
        else {
            unreachable!()
        };
        arma.mission_cycle.push(Arma3Mission {
            enabled: true,
            source: None,
            params: Default::default(),
            template: "Example.World".into(),
            difficulty: Arma3Difficulty::Veteran,
        });
        assert!(ServerDefinition::Arma3(arma.clone()).validate().is_ok());
        for template in [
            ".",
            "Example.",
            ".World",
            "../Example.World",
            "Example/World",
            "Example.World;quit",
        ] {
            arma.mission_cycle[0].template = template.into();
            assert!(ServerDefinition::Arma3(arma.clone()).validate().is_err());
        }
        arma.mission_cycle.clear();
        arma.game_port = 0;
        assert!(ServerDefinition::Arma3(arma).validate().is_err());
        let ServerDefinition::Reforger(mut reforger) =
            ServerDefinition::draft(ServerDefinitionKind::Reforger, "MainServer")
        else {
            unreachable!()
        };
        reforger.scenario_id = Some("{0123456789ABCDEF}Missions/Example.conf".into());
        reforger.max_players = 128;
        assert!(
            ServerDefinition::Reforger(reforger.clone())
                .validate()
                .is_ok()
        );
        reforger.max_players = 129;
        assert!(
            ServerDefinition::Reforger(reforger.clone())
                .validate()
                .is_err()
        );
        reforger.max_players = 64;
        reforger.a2s_port = Some(reforger.bind_port);
        assert!(
            ServerDefinition::Reforger(reforger.clone())
                .validate()
                .is_err()
        );
        reforger.a2s_port = Some(0);
        assert!(ServerDefinition::Reforger(reforger).validate().is_err());
    }

    #[test]
    fn a_mission_template_cannot_name_a_script() {
        let mut instance =
            ServerInstance::draft("alpha", ServerDefinitionKind::Arma3, "MainServer").unwrap();
        {
            let ServerDefinition::Arma3(settings) = &mut instance.definition else {
                unreachable!()
            };
            settings.mission_cycle.push(Arma3Mission {
                enabled: true,
                source: None,
                params: Default::default(),
                template: "boot.sqf".into(),
                difficulty: Arma3Difficulty::Regular,
            });
        }
        assert!(instance.definition.validate().is_err());
        {
            let ServerDefinition::Arma3(settings) = &mut instance.definition else {
                unreachable!()
            };
            settings.mission_cycle[0].template = "boot.Altis".into();
        }
        assert!(instance.definition.validate().is_ok());
    }
}
