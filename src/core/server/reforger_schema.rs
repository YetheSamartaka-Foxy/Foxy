use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use super::store::{ServerStore, ServerStoreError, check_directory_chain, check_regular_file};
use super::{
    ReforgerAddonRequirement, ReforgerGameProperties, ReforgerPreservedJson,
    ReforgerServerDefinition, ServerDefinition, ServerDefinitionKind, ServerInstanceId,
};

pub const SCENARIO_DISCOVERY_SCHEMA: u32 = 1;
pub const SCENARIO_PARSER_VERSION: u32 = 1;
const MAX_SERVER_JSON_BYTES: usize = 1024 * 1024;
const MAX_SCENARIO_LOG_BYTES: u64 = 32 * 1024 * 1024;
const MAX_JSON_DEPTH: usize = 8;
const MAX_OBJECT_KEYS: usize = 128;
const MAX_ARRAY_LEN: usize = 256;
const MAX_STRING_BYTES: usize = 8192;
const MAX_SECRET_PATHS: usize = 64;

const ROOT_OWNED: &[&str] = &[
    "bindAddress",
    "bindPort",
    "a2s",
    "rcon",
    "game",
    "operating",
];
const A2S_OWNED: &[&str] = &["address", "port"];
const GAME_OWNED: &[&str] = &[
    "name",
    "password",
    "passwordAdmin",
    "scenarioId",
    "maxPlayers",
    "visible",
    "crossPlatform",
    "gameProperties",
    "modsRequiredByDefault",
    "mods",
];
const PROPERTY_OWNED: &[&str] = &[
    "serverMaxViewDistance",
    "serverMinGrassDistance",
    "networkViewDistance",
    "disableThirdPerson",
    "fastValidation",
    "battlEye",
];
const MOD_OWNED: &[&str] = &["modId", "version", "required"];
const RCON_TYPED: &[&str] = &["address", "port", "permission", "maxClients"];

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReforgerImportReview {
    pub definition: ReforgerServerDefinition,
    pub warnings: Vec<String>,
    pub unknown_paths: Vec<String>,
    pub redacted_secret_paths: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScenarioListingStatus {
    Complete,
    CompleteEmpty,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiscoveredScenario {
    pub resource: String,
    pub label: Option<String>,
    pub provider: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ParsedScenarioListing {
    pub status: ScenarioListingStatus,
    pub scenarios: Vec<DiscoveredScenario>,
    pub diagnostics: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScenarioDiscovery {
    pub schema_version: u32,
    pub game_space_id: String,
    pub instance_id: ServerInstanceId,
    pub parser_version: u32,
    pub status: ScenarioListingStatus,
    pub scenarios: Vec<DiscoveredScenario>,
    pub diagnostics: Vec<String>,
}

pub fn import_server_json(bytes: &[u8]) -> Result<ReforgerImportReview, String> {
    if bytes.len() > MAX_SERVER_JSON_BYTES {
        return Err("Reforger server.json exceeds 1 MiB".to_string());
    }
    if bytes.is_empty() {
        return Err("Reforger server.json is empty".to_string());
    }
    let value: Value = serde_json::from_slice(bytes)
        .map_err(|_| "Reforger server.json is not valid JSON".to_string())?;
    bounded_json(&value, 0)?;
    let root = value
        .as_object()
        .ok_or_else(|| "Reforger server.json must be a JSON object".to_string())?;
    let mut warnings = Vec::new();
    let mut unknown_paths = Vec::new();
    let mut redacted = Vec::new();
    let mut preserved = ReforgerPreservedJson::default();

    let bind_port = required_u16(root, "bindPort")?;
    let bind_address = optional_ipv4(root, "bindAddress", &mut warnings)?;
    let query = import_a2s(
        root.get("a2s"),
        &mut warnings,
        &mut redacted,
        &mut unknown_paths,
    )?;
    let a2s_port = query.port;
    let bind_address = match (bind_address, query.address) {
        (Some(bind), Some(query)) if bind != query => {
            warnings.push(
                "a2s.address differed from bindAddress. The draft keeps bindAddress and renders both from it."
                    .to_string(),
            );
            Some(bind)
        }
        (None, Some(query)) => {
            warnings.push(
                "bindAddress was taken from a2s.address because bindAddress was absent."
                    .to_string(),
            );
            Some(query)
        }
        (bind, _) => bind,
    };
    preserved.a2s = query.preserved;
    let mut rcon_settings = None;
    if let Some(rcon) = root.get("rcon") {
        let mut map = import_object_map(rcon, "rcon", &mut redacted, &mut unknown_paths)?;
        match typed_rcon(&map, bind_port, a2s_port) {
            Some(settings) => {
                for key in RCON_TYPED {
                    map.remove(*key);
                }
                rcon_settings = Some(settings);
                warnings.push("RCon settings were imported without a password. Store one with Set RCon password before starting.".to_string());
            }
            None => warnings.push("The rcon block was kept as imported values because its address, port, permission or client limit is not in a supported form.".to_string()),
        }
        preserved.rcon = map;
    }
    if let Some(operating) = root.get("operating") {
        preserved.operating = Some(import_object_value(
            operating,
            "operating",
            &mut redacted,
            &mut unknown_paths,
        )?);
    }
    let game = root
        .get("game")
        .and_then(Value::as_object)
        .ok_or_else(|| "Reforger game must be a JSON object".to_string())?;
    let name = required_string(game, "name")?;
    let max_players = required_u16(game, "maxPlayers")?;
    let visible = required_bool(game, "visible")?;
    let scenario_id = optional_string(game, "scenarioId", &mut warnings)?;
    let cross_platform = optional_bool(game, "crossPlatform", &mut warnings)?;
    let mods_required_by_default = optional_bool(game, "modsRequiredByDefault", &mut warnings)?;
    let (addons, mods_absent) = import_mods(
        game.get("mods"),
        &mut warnings,
        &mut redacted,
        &mut unknown_paths,
    )?;
    if mods_absent {
        warnings.push("game.mods was absent and was stored as an empty addon list.".to_string());
    }
    let (game_properties, properties_preserved) = import_properties(
        game.get("gameProperties"),
        &mut warnings,
        &mut redacted,
        &mut unknown_paths,
    )?;
    preserved.game_properties = properties_preserved;
    preserved.game = unknown_map(game, "game", GAME_OWNED, &mut redacted, &mut unknown_paths)?;
    if preserved.game.contains_key("crossplay") {
        warnings.push(
            "The imported crossplay alias was preserved and was not rewritten as crossPlatform."
                .to_string(),
        );
    }
    if preserved.game.contains_key("admins") {
        warnings.push("Admin identities were preserved in the draft.".to_string());
    }
    if secret_key(game, "password") || secret_key(game, "passwordAdmin") {
        if game.contains_key("password") {
            redacted.push("game.password".to_string());
        }
        if game.contains_key("passwordAdmin") {
            redacted.push("game.passwordAdmin".to_string());
        }
    }
    preserved.root = unknown_map(root, "", ROOT_OWNED, &mut redacted, &mut unknown_paths)?;
    if preserved.root.contains_key("persistence") {
        warnings.push(
            "A root persistence object was preserved and was not moved into game.gameProperties."
                .to_string(),
        );
    }
    redacted.sort();
    redacted.dedup();
    if redacted.len() > MAX_SECRET_PATHS {
        return Err("Reforger server.json contains too many secret fields".to_string());
    }
    if !redacted.is_empty() {
        warnings.push(
            "Secret values were removed during import and are not in this preview.".to_string(),
        );
    }
    let definition = ReforgerServerDefinition {
        name,
        bind_port,
        bind_address,
        a2s_port,
        max_players,
        visible,
        scenario_id,
        addons,
        cross_platform,
        mods_required_by_default,
        game_properties,
        preserved,
        redacted_secret_paths: redacted.clone(),
        rcon: rcon_settings,
    };
    ServerDefinition::Reforger(Box::new(definition.clone())).validate()?;
    unknown_paths.sort();
    unknown_paths.dedup();
    warnings.sort();
    warnings.dedup();
    Ok(ReforgerImportReview {
        definition,
        warnings,
        unknown_paths,
        redacted_secret_paths: redacted,
    })
}

pub fn render_server_json(settings: &ReforgerServerDefinition) -> Value {
    let addons = settings.addons.iter().map(render_addon).collect::<Vec<_>>();
    let mut value = json!({
        "bindPort": settings.bind_port,
        "game": {
            "name": settings.name,
            "maxPlayers": settings.max_players,
            "visible": settings.visible,
            "mods": addons
        }
    });
    if let Some(address) = settings.bind_address {
        value["bindAddress"] = json!(address);
    }
    if settings.a2s_port.is_some() || !settings.preserved.a2s.is_empty() {
        let mut query = Map::new();
        if let Some(port) = settings.a2s_port {
            query.insert("port".to_string(), json!(port));
            if let Some(address) = settings.bind_address {
                query.insert("address".to_string(), json!(address));
            }
        }
        insert_preserved(&mut query, &settings.preserved.a2s);
        value["a2s"] = Value::Object(query);
    }
    if let Some(scenario) = &settings.scenario_id {
        value["game"]["scenarioId"] = scenario.clone().into();
    }
    if let Some(cross_platform) = settings.cross_platform {
        value["game"]["crossPlatform"] = json!(cross_platform);
    }
    if let Some(required) = settings.mods_required_by_default {
        value["game"]["modsRequiredByDefault"] = json!(required);
    }
    if let Some(properties) = render_properties(
        &settings.game_properties,
        &settings.preserved.game_properties,
    ) {
        value["game"]["gameProperties"] = properties;
    }
    if let Some(game) = value.get_mut("game").and_then(Value::as_object_mut) {
        insert_preserved(game, &settings.preserved.game);
    }
    if settings.rcon.is_some() || !settings.preserved.rcon.is_empty() {
        let mut rcon = Map::new();
        if let Some(typed) = &settings.rcon {
            rcon.insert("address".into(), typed.address.to_string().into());
            rcon.insert("port".into(), typed.port.into());
            rcon.insert("permission".into(), typed.permission.native().into());
            if let Some(clients) = typed.max_clients {
                rcon.insert("maxClients".into(), clients.into());
            }
        }
        insert_preserved(&mut rcon, &settings.preserved.rcon);
        value["rcon"] = Value::Object(rcon);
    }
    if let Some(operating) = &settings.preserved.operating {
        value["operating"] = operating.clone();
    }
    if let Some(root) = value.as_object_mut() {
        insert_preserved(root, &settings.preserved.root);
    }
    strip_secret_keys(&mut value);
    value
}

pub fn preview_warnings(settings: &ReforgerServerDefinition) -> Vec<String> {
    let mut warnings = Vec::new();
    if settings.scenario_id.is_none() {
        warnings.push("No Reforger scenario resource is configured.".to_string());
    } else {
        warnings.push(
            "A scenario resource name does not prove that the scenario is installed.".to_string(),
        );
    }
    if !settings.redacted_secret_paths.is_empty() {
        warnings.push(
            "Secret values were removed during import and are not in this preview.".to_string(),
        );
    }
    match settings.rcon.as_ref().map(|rcon| rcon.password_ref.is_some()) {
        Some(true) => warnings.push("The RCon password is protected. It is written only into private runtime state when the server starts.".to_string()),
        Some(false) => warnings.push("RCon is configured without a password. Set the RCon password before this configuration can start.".to_string()),
        None => {}
    }
    if !settings.addons.is_empty() {
        warnings.push(
            "Addon rows are desired intent, not acquired content. Native activation still requires an empty addon list."
                .to_string(),
        );
    }
    if settings.addons.iter().any(|addon| addon.version.is_none()) && !settings.addons.is_empty() {
        warnings.push(
            "An omitted addon version means latest intent, not a pinned version.".to_string(),
        );
    }
    if !settings.preserved.is_empty()
        || settings
            .addons
            .iter()
            .any(|addon| !addon.preserved.is_empty())
    {
        warnings.push(
            "Preserved server.json fields are included. Review them before activation.".to_string(),
        );
    }
    if settings.preserved.root.contains_key("persistence") {
        warnings.push(
            "A root persistence object was preserved and was not moved into game.gameProperties."
                .to_string(),
        );
    }
    if settings.preserved.game.contains_key("crossplay") {
        warnings.push(
            "The imported crossplay alias was preserved and was not rewritten as crossPlatform."
                .to_string(),
        );
    }
    if settings.preserved.game.contains_key("admins") {
        warnings.push("Admin identities were preserved in the draft.".to_string());
    }
    warnings
}

pub fn scenario_listing_note(
    selected: Option<&str>,
    discovery: Option<&ScenarioDiscovery>,
) -> &'static str {
    let Some(discovery) = discovery else {
        return "No scenario listing is stored.";
    };
    let Some(selected) = selected.filter(|value| !value.is_empty()) else {
        return "No scenario is selected. The stored listing does not choose one.";
    };
    if discovery
        .scenarios
        .iter()
        .any(|scenario| scenario.resource == selected)
    {
        "The selected scenario is in the stored listing."
    } else {
        "The selected scenario is not in the stored listing."
    }
}

pub fn parse_scenario_listing(output: &str) -> Result<ParsedScenarioListing, String> {
    if output.len() > MAX_SCENARIO_LOG_BYTES as usize {
        return Err("Scenario listing log exceeds 32 MiB".to_string());
    }
    let lines: Vec<_> = output.lines().collect();
    let mut diagnostics = Vec::new();
    let mut selected = None;
    for (index, line) in lines.iter().enumerate() {
        let Some(count) = script_message(line).and_then(official_scenario_count) else {
            continue;
        };
        if count > MAX_ARRAY_LEN {
            return Err("Official scenario listing exceeds 256 entries".to_string());
        }
        let mut start = index + 1;
        if lines.get(start).and_then(|line| script_message(line))
            == Some("--------------------------------------------------")
        {
            start += 1;
        }
        let mut scenarios = Vec::new();
        for line in &lines[start..] {
            let Some(message) = script_message(line) else {
                break;
            };
            match scenario_resource(message) {
                Some((resource, label)) => scenarios.push(DiscoveredScenario {
                    resource,
                    label,
                    provider: "official".to_string(),
                }),
                None => break,
            }
        }
        let unique = scenarios
            .iter()
            .map(|scenario| scenario.resource.clone())
            .collect::<std::collections::BTreeSet<_>>();
        if scenarios.len() == count && unique.len() == count {
            selected = Some((index, start, scenarios));
            break;
        }
        if diagnostics.len() < 8 {
            diagnostics.push(
                "An incomplete official scenario block was ignored and did not replace a listing."
                    .to_string(),
            );
        }
    }
    let Some((index, start, scenarios)) = selected else {
        return Err(
            "No complete official scenario listing was found. Nothing was stored.".to_string(),
        );
    };
    let count = scenarios.len();
    let mut outside = 0usize;
    for line in lines
        .iter()
        .take(index)
        .chain(lines.iter().skip(start + scenarios.len()))
    {
        if script_message(line).is_some_and(mentions_unscoped_scenario) {
            outside += 1;
        }
    }
    if outside > 0 {
        diagnostics.push(format!(
            "{outside} scenario-like lines were outside the official listing and were not added"
        ));
    }
    let status = if count == 0 {
        ScenarioListingStatus::CompleteEmpty
    } else {
        ScenarioListingStatus::Complete
    };
    Ok(ParsedScenarioListing {
        status,
        scenarios,
        diagnostics,
    })
}

pub(super) fn validate_reforger(settings: &ReforgerServerDefinition) -> Result<(), String> {
    validate_distance(
        settings.game_properties.server_max_view_distance,
        "serverMaxViewDistance",
    )?;
    validate_distance(
        settings.game_properties.server_min_grass_distance,
        "serverMinGrassDistance",
    )?;
    validate_distance(
        settings.game_properties.network_view_distance,
        "networkViewDistance",
    )?;
    if settings.addons.len() > MAX_ARRAY_LEN {
        return Err("Reforger addon intent exceeds 256 rows".to_string());
    }
    if settings.redacted_secret_paths.len() > MAX_SECRET_PATHS {
        return Err("Reforger secret-path record exceeds 64 entries".to_string());
    }
    for path in &settings.redacted_secret_paths {
        if path.is_empty() || path.len() > 256 || path.chars().any(char::is_control) {
            return Err("Invalid Reforger secret-path record".to_string());
        }
    }
    validate_map(&settings.preserved.root, ROOT_OWNED)?;
    validate_map(&settings.preserved.a2s, A2S_OWNED)?;
    validate_map(&settings.preserved.game, GAME_OWNED)?;
    validate_map(&settings.preserved.game_properties, PROPERTY_OWNED)?;
    validate_map(&settings.preserved.rcon, &[])?;
    if let Some(operating) = &settings.preserved.operating {
        bounded_json(operating, 0)?;
        if !operating.is_object() {
            return Err("Preserved Reforger operating value must be an object".to_string());
        }
        if contains_secret(operating) {
            return Err("Preserved Reforger JSON still contains a secret field".to_string());
        }
    }
    for addon in &settings.addons {
        validate_map(&addon.preserved, MOD_OWNED)?;
    }
    Ok(())
}

impl ServerStore {
    pub fn read_scenario_discovery(
        &self,
        id: &ServerInstanceId,
    ) -> Result<Option<ScenarioDiscovery>, ServerStoreError> {
        let path = self.scenario_discovery_path(id);
        check_directory_chain(path.parent().ok_or(ServerStoreError::UnsafePath)?)?;
        check_regular_file(&path)?;
        let bytes = match super::read_bounded(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(ServerStoreError::Io),
        };
        let discovery: ScenarioDiscovery =
            serde_json::from_slice(&bytes).map_err(|_| ServerStoreError::Corrupt)?;
        self.validate_scenario_discovery(id, &discovery)?;
        Ok(Some(discovery))
    }

    pub fn write_scenario_discovery(
        &self,
        id: &ServerInstanceId,
        listing: &ParsedScenarioListing,
    ) -> Result<ScenarioDiscovery, ServerStoreError> {
        let instance = self.show(id)?;
        if instance.definition.kind() != ServerDefinitionKind::Reforger {
            return Err(ServerStoreError::UnsupportedGame);
        }
        let discovery = ScenarioDiscovery {
            schema_version: SCENARIO_DISCOVERY_SCHEMA,
            game_space_id: self.context().space_id().to_string(),
            instance_id: id.clone(),
            parser_version: SCENARIO_PARSER_VERSION,
            status: listing.status,
            scenarios: listing.scenarios.clone(),
            diagnostics: listing.diagnostics.clone(),
        };
        self.validate_scenario_discovery(id, &discovery)?;
        let path = self.scenario_discovery_path(id);
        let (catalog, _, _) = self.read()?;
        if catalog
            .installations
            .iter()
            .any(|installation| super::installation::paths_overlap(&installation.directory, &path))
        {
            return Err(ServerStoreError::UnsafePath);
        }
        let bytes = serde_json::to_vec_pretty(&discovery).map_err(|_| ServerStoreError::Corrupt)?;
        if bytes.len() > MAX_SERVER_JSON_BYTES {
            return Err(ServerStoreError::Invalid(
                "Scenario listing exceeds 1 MiB".into(),
            ));
        }
        let parent = path.parent().ok_or(ServerStoreError::UnsafePath)?;
        check_directory_chain(parent)?;
        check_regular_file(&path)?;
        fs::create_dir_all(parent).map_err(|_| ServerStoreError::Io)?;
        check_directory_chain(parent)?;
        let mut temporary =
            tempfile::NamedTempFile::new_in(parent).map_err(|_| ServerStoreError::Io)?;
        std::io::Write::write_all(&mut temporary, &bytes)
            .and_then(|()| temporary.as_file().sync_all())
            .map_err(|_| ServerStoreError::Io)?;
        check_directory_chain(parent)?;
        check_regular_file(&path)?;
        temporary.persist(&path).map_err(|_| ServerStoreError::Io)?;
        Ok(discovery)
    }

    fn scenario_discovery_path(&self, id: &ServerInstanceId) -> PathBuf {
        self.context()
            .space_dir()
            .join("server/instances")
            .join(id.as_str())
            .join("scenario-discovery.json")
    }

    fn validate_scenario_discovery(
        &self,
        id: &ServerInstanceId,
        discovery: &ScenarioDiscovery,
    ) -> Result<(), ServerStoreError> {
        if discovery.schema_version != SCENARIO_DISCOVERY_SCHEMA
            || discovery.parser_version != SCENARIO_PARSER_VERSION
            || discovery.game_space_id != self.context().space_id()
            || &discovery.instance_id != id
            || discovery.scenarios.len() > MAX_ARRAY_LEN
            || discovery.diagnostics.len() > 32
        {
            return Err(ServerStoreError::Corrupt);
        }
        let empty = discovery.scenarios.is_empty();
        if (discovery.status == ScenarioListingStatus::CompleteEmpty) != empty {
            return Err(ServerStoreError::Corrupt);
        }
        let mut seen = std::collections::BTreeSet::new();
        for scenario in &discovery.scenarios {
            if scenario.provider != "official"
                || !seen.insert(scenario.resource.clone())
                || !super::definition::scenario_resource_valid(&scenario.resource)
            {
                return Err(ServerStoreError::Corrupt);
            }
            if let Some(label) = &scenario.label
                && (label.is_empty() || label.len() > 256 || label.chars().any(char::is_control))
            {
                return Err(ServerStoreError::Corrupt);
            }
        }
        Ok(())
    }
}

pub fn write_server_json(path: &Path, value: &Value) -> Result<(), String> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    check_directory_chain(parent)
        .map_err(|_| "Export directory must be an ordinary directory".to_string())?;
    let bytes =
        serde_json::to_vec_pretty(value).map_err(|_| "Cannot encode server.json".to_string())?;
    if bytes.len() > MAX_SERVER_JSON_BYTES {
        return Err("Rendered Reforger server.json exceeds 1 MiB".to_string());
    }
    let mut file = tempfile::NamedTempFile::new_in(parent)
        .map_err(|_| "Cannot create the export file".to_string())?;
    file.write_all(&bytes)
        .and_then(|()| file.as_file().sync_all())
        .map_err(|_| "Cannot write the export file".to_string())?;
    file.persist_noclobber(path)
        .map_err(|_| "Export destination already exists or cannot be created".to_string())?;
    Ok(())
}

pub fn read_scenario_log(path: &Path) -> Result<String, String> {
    check_regular_file(path)
        .map_err(|_| "Scenario listing log must be an ordinary file".to_string())?;
    let mut bytes = Vec::new();
    File::open(path)
        .and_then(|file| {
            file.take(MAX_SCENARIO_LOG_BYTES + 1)
                .read_to_end(&mut bytes)
        })
        .map_err(|_| "Cannot read the scenario listing log".to_string())?;
    if bytes.len() as u64 > MAX_SCENARIO_LOG_BYTES {
        return Err("Scenario listing log exceeds 32 MiB".to_string());
    }
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

fn render_addon(addon: &ReforgerAddonRequirement) -> Value {
    let mut value = json!({"modId": addon.addon_id});
    if let Some(version) = &addon.version {
        value["version"] = version.clone().into();
    }
    if let Some(required) = addon.required {
        value["required"] = json!(required);
    }
    if let Some(object) = value.as_object_mut() {
        insert_preserved(object, &addon.preserved);
    }
    value
}

fn render_properties(
    properties: &ReforgerGameProperties,
    preserved: &BTreeMap<String, Value>,
) -> Option<Value> {
    let mut object = Map::new();
    insert_u32(
        &mut object,
        "serverMaxViewDistance",
        properties.server_max_view_distance,
    );
    insert_u32(
        &mut object,
        "serverMinGrassDistance",
        properties.server_min_grass_distance,
    );
    insert_u32(
        &mut object,
        "networkViewDistance",
        properties.network_view_distance,
    );
    insert_bool(
        &mut object,
        "disableThirdPerson",
        properties.disable_third_person,
    );
    insert_bool(&mut object, "fastValidation", properties.fast_validation);
    insert_bool(&mut object, "battlEye", properties.battleye);
    insert_preserved(&mut object, preserved);
    (!object.is_empty()).then_some(Value::Object(object))
}

fn insert_u32(object: &mut Map<String, Value>, key: &str, value: Option<u32>) {
    if let Some(value) = value {
        object.insert(key.to_string(), json!(value));
    }
}

fn insert_bool(object: &mut Map<String, Value>, key: &str, value: Option<bool>) {
    if let Some(value) = value {
        object.insert(key.to_string(), json!(value));
    }
}

fn insert_preserved(object: &mut Map<String, Value>, preserved: &BTreeMap<String, Value>) {
    for (key, value) in preserved {
        if is_secret_name(key) {
            continue;
        }
        object.entry(key.clone()).or_insert_with(|| value.clone());
    }
}

fn strip_secret_keys(value: &mut Value) {
    match value {
        Value::Object(object) => {
            object.retain(|key, _| !is_secret_name(key));
            for child in object.values_mut() {
                strip_secret_keys(child);
            }
        }
        Value::Array(items) => {
            for item in items {
                strip_secret_keys(item);
            }
        }
        _ => {}
    }
}

struct ImportedQuery {
    port: Option<u16>,
    address: Option<Ipv4Addr>,
    preserved: BTreeMap<String, Value>,
}

fn import_a2s(
    value: Option<&Value>,
    warnings: &mut Vec<String>,
    redacted: &mut Vec<String>,
    unknown_paths: &mut Vec<String>,
) -> Result<ImportedQuery, String> {
    let Some(value) = value else {
        return Ok(ImportedQuery {
            port: None,
            address: None,
            preserved: BTreeMap::new(),
        });
    };
    if value.is_null() {
        warnings.push("Null values for typed fields were omitted.".to_string());
        return Ok(ImportedQuery {
            port: None,
            address: None,
            preserved: BTreeMap::new(),
        });
    }
    let object = value
        .as_object()
        .ok_or_else(|| "Reforger a2s must be a JSON object".to_string())?;
    Ok(ImportedQuery {
        port: optional_u16(object, "port", warnings)?,
        address: optional_ipv4(object, "address", warnings)?,
        preserved: unknown_map(object, "a2s", A2S_OWNED, redacted, unknown_paths)?,
    })
}

fn import_mods(
    value: Option<&Value>,
    warnings: &mut Vec<String>,
    redacted: &mut Vec<String>,
    unknown_paths: &mut Vec<String>,
) -> Result<(Vec<ReforgerAddonRequirement>, bool), String> {
    let Some(value) = value else {
        return Ok((Vec::new(), true));
    };
    if value.is_null() {
        warnings.push("Null values for typed fields were omitted.".to_string());
        return Ok((Vec::new(), true));
    }
    let rows = value
        .as_array()
        .ok_or_else(|| "Reforger game.mods must be an array".to_string())?;
    if rows.len() > MAX_ARRAY_LEN {
        return Err("Reforger addon intent exceeds 256 rows".to_string());
    }
    let mut addons = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        let object = row
            .as_object()
            .ok_or_else(|| format!("Reforger addon {index} must be an object"))?;
        let addon_id = required_string(object, "modId")?;
        let version = optional_string(object, "version", warnings)?;
        let required = optional_bool(object, "required", warnings)?;
        let preserved = unknown_map(
            object,
            &format!("game.mods[{index}]"),
            MOD_OWNED,
            redacted,
            unknown_paths,
        )?;
        addons.push(ReforgerAddonRequirement {
            addon_id,
            version,
            required,
            preserved,
        });
    }
    Ok((addons, false))
}

fn import_properties(
    value: Option<&Value>,
    warnings: &mut Vec<String>,
    redacted: &mut Vec<String>,
    unknown_paths: &mut Vec<String>,
) -> Result<(ReforgerGameProperties, BTreeMap<String, Value>), String> {
    let Some(value) = value else {
        return Ok((ReforgerGameProperties::default(), BTreeMap::new()));
    };
    if value.is_null() {
        warnings.push("Null values for typed fields were omitted.".to_string());
        return Ok((ReforgerGameProperties::default(), BTreeMap::new()));
    }
    let object = value
        .as_object()
        .ok_or_else(|| "Reforger game.gameProperties must be an object".to_string())?;
    let properties = ReforgerGameProperties {
        server_max_view_distance: optional_u32(object, "serverMaxViewDistance", warnings)?,
        server_min_grass_distance: optional_u32(object, "serverMinGrassDistance", warnings)?,
        network_view_distance: optional_u32(object, "networkViewDistance", warnings)?,
        disable_third_person: optional_bool(object, "disableThirdPerson", warnings)?,
        fast_validation: optional_bool(object, "fastValidation", warnings)?,
        battleye: optional_bool(object, "battlEye", warnings)?,
    };
    let preserved = unknown_map(
        object,
        "game.gameProperties",
        PROPERTY_OWNED,
        redacted,
        unknown_paths,
    )?;
    Ok((properties, preserved))
}

fn typed_rcon(
    map: &BTreeMap<String, Value>,
    bind_port: u16,
    a2s_port: Option<u16>,
) -> Option<crate::core::server::ReforgerRconSettings> {
    let address = map.get("address")?.as_str()?.parse::<Ipv4Addr>().ok()?;
    let port = u16::try_from(map.get("port")?.as_u64()?).ok()?;
    let permission = match map.get("permission")?.as_str()? {
        "admin" => crate::core::server::ReforgerRconPermission::Admin,
        "monitor" => crate::core::server::ReforgerRconPermission::Monitor,
        _ => return None,
    };
    let max_clients = match map.get("maxClients") {
        None => None,
        Some(value) => Some(u8::try_from(value.as_u64()?).ok()?),
    };
    let settings = crate::core::server::ReforgerRconSettings {
        address,
        port,
        permission,
        max_clients,
        password_ref: None,
    };
    settings.validate(bind_port, a2s_port).ok()?;
    Some(settings)
}

fn import_object_map(
    value: &Value,
    path: &str,
    redacted: &mut Vec<String>,
    unknown_paths: &mut Vec<String>,
) -> Result<BTreeMap<String, Value>, String> {
    let object = value
        .as_object()
        .ok_or_else(|| format!("Reforger {path} must be an object"))?;
    unknown_map(object, path, &[], redacted, unknown_paths)
}

fn import_object_value(
    value: &Value,
    path: &str,
    redacted: &mut Vec<String>,
    unknown_paths: &mut Vec<String>,
) -> Result<Value, String> {
    if !value.is_object() {
        return Err(format!("Reforger {path} must be an object"));
    }
    unknown_paths.push(path.to_string());
    keep_value(value, path, redacted)?.ok_or_else(|| format!("Reforger {path} must be an object"))
}

fn unknown_map(
    object: &Map<String, Value>,
    path: &str,
    owned: &[&str],
    redacted: &mut Vec<String>,
    unknown_paths: &mut Vec<String>,
) -> Result<BTreeMap<String, Value>, String> {
    let mut preserved = BTreeMap::new();
    for (key, value) in object {
        if owned.contains(&key.as_str()) || is_secret_name(key) {
            if is_secret_name(key) {
                redacted.push(join_path(path, key));
            }
            continue;
        }
        let child = join_path(path, key);
        unknown_paths.push(child.clone());
        if let Some(cleaned) = keep_value(value, &child, redacted)? {
            preserved.insert(key.clone(), cleaned);
        }
    }
    Ok(preserved)
}

fn keep_value(
    value: &Value,
    path: &str,
    redacted: &mut Vec<String>,
) -> Result<Option<Value>, String> {
    bounded_json(value, 0)?;
    Ok(strip_secrets(value, path, redacted))
}

fn strip_secrets(value: &Value, path: &str, redacted: &mut Vec<String>) -> Option<Value> {
    match value {
        Value::Object(object) => {
            let mut cleaned = Map::new();
            for (key, child) in object {
                let child_path = join_path(path, key);
                if is_secret_name(key) {
                    redacted.push(child_path);
                    continue;
                }
                if let Some(cleaned_child) = strip_secrets(child, &child_path, redacted) {
                    cleaned.insert(key.clone(), cleaned_child);
                }
            }
            Some(Value::Object(cleaned))
        }
        Value::Array(items) => Some(Value::Array(
            items
                .iter()
                .enumerate()
                .filter_map(|(index, item)| {
                    strip_secrets(item, &format!("{path}[{index}]"), redacted)
                })
                .collect(),
        )),
        _ => Some(value.clone()),
    }
}

fn join_path(path: &str, key: &str) -> String {
    if path.is_empty() {
        key.to_string()
    } else {
        format!("{path}.{key}")
    }
}

fn required_u16(object: &Map<String, Value>, key: &str) -> Result<u16, String> {
    let value = object
        .get(key)
        .ok_or_else(|| format!("Reforger {key} is required"))?;
    json_u16(value, key)
}

fn optional_u16(
    object: &Map<String, Value>,
    key: &str,
    warnings: &mut Vec<String>,
) -> Result<Option<u16>, String> {
    match object.get(key) {
        None => Ok(None),
        Some(value) if value.is_null() => {
            warnings.push("Null values for typed fields were omitted.".to_string());
            Ok(None)
        }
        Some(value) => json_u16(value, key).map(Some),
    }
}

fn optional_u32(
    object: &Map<String, Value>,
    key: &str,
    warnings: &mut Vec<String>,
) -> Result<Option<u32>, String> {
    match object.get(key) {
        None => Ok(None),
        Some(value) if value.is_null() => {
            warnings.push("Null values for typed fields were omitted.".to_string());
            Ok(None)
        }
        Some(value) => json_u32(value, key).map(Some),
    }
}

fn required_bool(object: &Map<String, Value>, key: &str) -> Result<bool, String> {
    let value = object
        .get(key)
        .ok_or_else(|| format!("Reforger {key} is required"))?;
    value
        .as_bool()
        .ok_or_else(|| format!("Reforger {key} must be a boolean"))
}

fn optional_bool(
    object: &Map<String, Value>,
    key: &str,
    warnings: &mut Vec<String>,
) -> Result<Option<bool>, String> {
    match object.get(key) {
        None => Ok(None),
        Some(value) if value.is_null() => {
            warnings.push("Null values for typed fields were omitted.".to_string());
            Ok(None)
        }
        Some(value) => value
            .as_bool()
            .map(Some)
            .ok_or_else(|| format!("Reforger {key} must be a boolean")),
    }
}

fn required_string(object: &Map<String, Value>, key: &str) -> Result<String, String> {
    let value = object
        .get(key)
        .ok_or_else(|| format!("Reforger {key} is required"))?;
    value
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| format!("Reforger {key} must be a string"))
}

fn optional_string(
    object: &Map<String, Value>,
    key: &str,
    warnings: &mut Vec<String>,
) -> Result<Option<String>, String> {
    match object.get(key) {
        None => Ok(None),
        Some(value) if value.is_null() => {
            warnings.push("Null values for typed fields were omitted.".to_string());
            Ok(None)
        }
        Some(value) => value
            .as_str()
            .map(|text| Some(text.to_string()))
            .ok_or_else(|| format!("Reforger {key} must be a string")),
    }
}

fn optional_ipv4(
    object: &Map<String, Value>,
    key: &str,
    warnings: &mut Vec<String>,
) -> Result<Option<Ipv4Addr>, String> {
    match object.get(key) {
        None => Ok(None),
        Some(value) if value.is_null() => {
            warnings.push("Null values for typed fields were omitted.".to_string());
            Ok(None)
        }
        Some(value) => {
            let text = value
                .as_str()
                .ok_or_else(|| format!("Reforger {key} must be an IPv4 address"))?;
            text.parse::<Ipv4Addr>()
                .map(Some)
                .map_err(|_| format!("Reforger {key} must be an IPv4 address"))
        }
    }
}

fn json_u16(value: &Value, key: &str) -> Result<u16, String> {
    let number = value
        .as_u64()
        .ok_or_else(|| format!("Reforger {key} must be an integer"))?;
    u16::try_from(number).map_err(|_| format!("Reforger {key} must be 0-65535"))
}

fn json_u32(value: &Value, key: &str) -> Result<u32, String> {
    let number = value
        .as_u64()
        .ok_or_else(|| format!("Reforger {key} must be an integer"))?;
    u32::try_from(number).map_err(|_| format!("Reforger {key} is outside the supported range"))
}

fn secret_key(object: &Map<String, Value>, key: &str) -> bool {
    object.contains_key(key) && is_secret_name(key)
}

fn is_secret_name(key: &str) -> bool {
    let lower = key.to_ascii_lowercase();
    lower.contains("password") || lower.contains("secret") || lower.contains("token")
}

fn contains_secret(value: &Value) -> bool {
    match value {
        Value::Object(object) => object
            .iter()
            .any(|(key, child)| is_secret_name(key) || contains_secret(child)),
        Value::Array(items) => items.iter().any(contains_secret),
        _ => false,
    }
}

fn validate_distance(value: Option<u32>, field: &str) -> Result<(), String> {
    if value.is_some_and(|value| value > 100_000) {
        return Err(format!("{field} must be 0-100000 meters"));
    }
    Ok(())
}

fn validate_map(map: &BTreeMap<String, Value>, owned: &[&str]) -> Result<(), String> {
    if map.len() > MAX_OBJECT_KEYS {
        return Err("Preserved Reforger JSON has too many fields".to_string());
    }
    for (key, value) in map {
        if owned.contains(&key.as_str()) || is_secret_name(key) {
            return Err(format!(
                "Preserved Reforger field {key} conflicts with a typed or secret field"
            ));
        }
        bounded_json(value, 0)?;
        if contains_secret(value) {
            return Err("Preserved Reforger JSON still contains a secret field".to_string());
        }
    }
    Ok(())
}

fn bounded_json(value: &Value, depth: usize) -> Result<(), String> {
    if depth > MAX_JSON_DEPTH {
        return Err("Reforger JSON nesting exceeds 8 levels".to_string());
    }
    match value {
        Value::Object(object) => {
            if object.len() > MAX_OBJECT_KEYS {
                return Err("Reforger JSON object has too many fields".to_string());
            }
            for (key, child) in object {
                if key.is_empty() || key.len() > 128 || key.chars().any(char::is_control) {
                    return Err("Reforger JSON field name is invalid".to_string());
                }
                bounded_json(child, depth + 1)?;
            }
        }
        Value::Array(items) => {
            if items.len() > MAX_ARRAY_LEN {
                return Err("Reforger JSON array exceeds 256 entries".to_string());
            }
            for item in items {
                bounded_json(item, depth + 1)?;
            }
        }
        Value::String(text) => {
            if text.len() > MAX_STRING_BYTES || text.chars().any(char::is_control) {
                return Err("Reforger JSON string is too long or contains controls".to_string());
            }
        }
        Value::Number(number) => {
            if number.to_string().len() > 32 {
                return Err("Reforger JSON number is outside the supported range".to_string());
            }
        }
        Value::Bool(_) | Value::Null => {}
    }
    Ok(())
}

fn mentions_unscoped_scenario(message: &str) -> bool {
    message.contains("}Missions/") && scenario_resource(message).is_none()
}

fn script_message(line: &str) -> Option<&str> {
    let script = line.find("SCRIPT")?;
    line[script + "SCRIPT".len()..]
        .split_once(':')
        .map(|(_, message)| message.trim())
}

fn official_scenario_count(message: &str) -> Option<usize> {
    let count = message
        .strip_prefix("Official scenarios (")?
        .strip_suffix(" entries)")?;
    (count.bytes().all(|byte| byte.is_ascii_digit()) && count.len() <= 6)
        .then_some(count.parse().ok())
        .flatten()
}

fn scenario_resource(message: &str) -> Option<(String, Option<String>)> {
    let (body, label) = match message.rsplit_once(" (") {
        Some((body, label)) => {
            let label = label.strip_suffix(')')?;
            if label.is_empty()
                || label.len() > 256
                || label
                    .chars()
                    .any(|character| character.is_control() || matches!(character, '(' | ')'))
            {
                return None;
            }
            (body, Some(label.to_string()))
        }
        None => (message, None),
    };
    let after_guid = body.strip_prefix('{')?;
    let (guid, resource) = after_guid.split_once('}')?;
    if guid.len() != 16 || !guid.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    let path = resource.strip_prefix("Missions/")?;
    let valid = path.ends_with(".conf")
        && !path.is_empty()
        && path.len() <= 1000
        && !path.starts_with('/')
        && !path.contains("..")
        && !path.contains(['\\', ':', '{', '}'])
        && !path.chars().any(char::is_control);
    valid.then(|| (format!("{{{guid}}}{resource}"), label))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_rcon(rcon: &str) -> Vec<u8> {
        format!(
            r#"{{"bindPort":2001,"bindAddress":"127.0.0.1","a2s":{{"address":"127.0.0.1","port":17777}},"rcon":{rcon},"game":{{"name":"A","maxPlayers":8,"visible":false,"scenarioId":"{{2BBBE828037C6F4B}}Missions/22_GM_Arland.conf","mods":[]}}}}"#
        )
        .into_bytes()
    }

    #[test]
    fn typed_rcon_settings_are_lifted_and_the_password_is_never_kept() {
        let review = import_server_json(&with_rcon(
            r##"{"address":"127.0.0.1","port":19999,"password":"do-not-keep","permission":"admin","maxClients":4,"blacklist":["#kick"],"futureKey":1}"##,
        ))
        .unwrap();
        let rcon = review.definition.rcon.clone().unwrap();
        assert_eq!(rcon.port, 19999);
        assert_eq!(
            rcon.permission,
            crate::core::server::ReforgerRconPermission::Admin
        );
        assert_eq!((rcon.max_clients, rcon.password_ref), (Some(4), None));
        assert_eq!(
            review.definition.preserved.rcon.keys().collect::<Vec<_>>(),
            ["blacklist", "futureKey"]
        );
        assert!(
            review
                .redacted_secret_paths
                .iter()
                .any(|path| path == "rcon.password")
        );
        assert!(
            review
                .warnings
                .iter()
                .any(|warning| warning.contains("without a password"))
        );
        let rendered = render_server_json(&review.definition);
        assert_eq!(rendered["rcon"]["port"], 19999);
        assert_eq!(rendered["rcon"]["permission"], "admin");
        assert_eq!(rendered["rcon"]["blacklist"], json!(["#kick"]));
        assert!(rendered["rcon"].get("password").is_none());
        for untyped in [
            r#"{"address":"rcon.example.invalid","port":19999,"permission":"admin"}"#,
            r#"{"address":"127.0.0.1","port":2001,"permission":"admin"}"#,
            r#"{"address":"127.0.0.1","port":19999,"permission":"owner"}"#,
            r#"{"address":"127.0.0.1","port":19999,"permission":"admin","maxClients":40}"#,
        ] {
            let review = import_server_json(&with_rcon(untyped)).unwrap();
            assert!(review.definition.rcon.is_none(), "{untyped}");
            assert!(review.definition.preserved.rcon.contains_key("port"));
        }
    }

    #[test]
    fn import_preserves_unknown_fields_explicit_values_and_strips_secrets() {
        let review = import_server_json(
            br#"{
                "bindPort": 2001,
                "bindAddress": "127.0.0.1",
                "publicAddress": "203.0.113.10",
                "persistence": {"saveRetention": 4},
                "a2s": {"address": "127.0.0.1", "port": 17777, "extra": true},
                "game": {
                    "name": "MainServer",
                    "password": "do-not-keep",
                    "maxPlayers": 64,
                    "visible": false,
                    "crossPlatform": false,
                    "crossplay": true,
                    "scenarioId": "{2BBBE828037C6F4B}Missions/22_GM_Arland.conf",
                    "modsRequiredByDefault": false,
                    "mods": [{"modId": "0123456789ABCDEF", "name": "Sample"}],
                    "gameProperties": {
                        "serverMaxViewDistance": 1600,
                        "serverMinGrassDistance": 0,
                        "battlEye": true,
                        "missionHeader": {"kept": true}
                    }
                }
            }"#,
        )
        .unwrap();
        assert_eq!(review.definition.cross_platform, Some(false));
        assert_eq!(review.definition.mods_required_by_default, Some(false));
        assert_eq!(
            review.definition.game_properties.server_min_grass_distance,
            Some(0)
        );
        assert_eq!(
            review.definition.preserved.root["publicAddress"],
            json!("203.0.113.10")
        );
        assert!(review.definition.preserved.root.contains_key("persistence"));
        assert_eq!(review.definition.preserved.a2s["extra"], json!(true));
        assert_eq!(
            review.definition.preserved.game_properties["missionHeader"]["kept"],
            json!(true)
        );
        assert_eq!(
            review.definition.addons[0].preserved["name"],
            json!("Sample")
        );
        let rendered = serde_json::to_string(&render_server_json(&review.definition)).unwrap();
        assert!(!rendered.contains("do-not-keep"));
        assert!(rendered.contains("missionHeader"));
        assert!(rendered.contains("203.0.113.10"));
        assert!(
            review
                .redacted_secret_paths
                .iter()
                .any(|path| path == "game.password")
        );
        let again = import_server_json(rendered.as_bytes()).unwrap();
        assert_eq!(
            again.definition.preserved.root["publicAddress"],
            json!("203.0.113.10")
        );
        assert!(again.definition.scenario_id.is_some());
    }

    #[test]
    fn invalid_scenario_and_partial_listing_do_not_become_a_catalog() {
        let error = import_server_json(
            br#"{"bindPort":2001,"game":{"name":"MainServer","maxPlayers":64,"visible":false,"scenarioId":"Example.conf","mods":[]}}"#,
        );
        assert!(error.is_err());
        assert!(
            parse_scenario_listing("SCRIPT : {0011223344556677}Missions/Noise.conf\n").is_err()
        );
        assert!(
            parse_scenario_listing(concat!(
                "SCRIPT : Official scenarios (2 entries)\n",
                "SCRIPT : {0011223344556677}Missions/OnlyOne.conf (Only one)\n",
            ))
            .is_err()
        );
        let recovered = parse_scenario_listing(concat!(
            "SCRIPT : Official scenarios (2 entries)\n",
            "SCRIPT : {0011223344556677}Missions/Torn.conf (Torn)\n",
            "SCRIPT : Official scenarios (1 entries)\n",
            "SCRIPT : --------------------------------------------------\n",
            "SCRIPT : {2BBBE828037C6F4B}Missions/22_GM_Arland.conf (Game Master)\n",
        ))
        .unwrap();
        assert_eq!(recovered.scenarios.len(), 1);
        assert_eq!(
            recovered.scenarios[0].resource,
            "{2BBBE828037C6F4B}Missions/22_GM_Arland.conf"
        );
        assert!(!recovered.diagnostics.is_empty());
        let listing = parse_scenario_listing(concat!(
            "SCRIPT : GetResourceObject \"{0011223344556677}Missions/Noise.conf\"\n",
            "08:43:25.153 SCRIPT       : Official scenarios (2 entries)\n",
            "08:43:25.153 SCRIPT       : --------------------------------------------------\n",
            "08:43:25.153 SCRIPT       : {0011223344556677}Missions/Conflict.conf (Conflict)\n",
            "08:43:25.153 SCRIPT       : {8899AABBCCDDEEFF}Missions/Tutorial.conf (Training)\n",
        ))
        .unwrap();
        assert_eq!(listing.scenarios.len(), 2);
        assert_eq!(
            listing.scenarios[0].resource,
            "{0011223344556677}Missions/Conflict.conf"
        );
        assert_eq!(listing.scenarios[0].label.as_deref(), Some("Conflict"));
        assert!(!listing.diagnostics.is_empty());
        let empty = parse_scenario_listing(
            "SCRIPT : Official scenarios (0 entries)\nSCRIPT : --------------------------------------------------\n",
        )
        .unwrap();
        assert_eq!(empty.status, ScenarioListingStatus::CompleteEmpty);
        assert!(empty.scenarios.is_empty());
    }

    #[test]
    fn example_server_json_round_trips_without_assuming_the_scenario_exists() {
        let review = import_server_json(include_bytes!(
            "../../../examples/json/server_backend/reforger_server.json"
        ))
        .unwrap();
        assert_eq!(
            review.definition.scenario_id.as_deref(),
            Some("{2BBBE828037C6F4B}Missions/22_GM_Arland.conf")
        );
        assert!(
            preview_warnings(&review.definition)
                .iter()
                .any(|warning| warning.contains("does not prove"))
        );
        let rendered = render_server_json(&review.definition);
        assert_eq!(
            rendered["game"]["gameProperties"]["serverMinGrassDistance"],
            0
        );
        assert_eq!(rendered["game"]["crossPlatform"], false);
    }
}
