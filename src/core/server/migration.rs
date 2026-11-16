use serde::Deserialize;

use super::definition::SERVER_SCHEMA_VERSION;
use super::{ServerCatalog, ServerDefinition, ServerInstance, ServerInstanceId, ServerStoreError};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CatalogV1 {
    schema_version: u32,
    game_space_id: String,
    game_id: String,
    revision: u64,
    instances: Vec<InstanceV1>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InstanceV1 {
    schema_version: u32,
    id: ServerInstanceId,
    game_space_id: String,
    revision: u64,
    display_name: String,
    definition: ServerDefinition,
}

pub(super) fn catalog(bytes: &[u8]) -> Result<ServerCatalog, ServerStoreError> {
    let old: CatalogV1 = serde_json::from_slice(bytes).map_err(|_| ServerStoreError::Corrupt)?;
    if old.schema_version != 1
        || old
            .instances
            .iter()
            .any(|instance| instance.schema_version != 1)
    {
        return Err(ServerStoreError::UnsupportedVersion);
    }
    Ok(ServerCatalog {
        schema_version: SERVER_SCHEMA_VERSION,
        game_space_id: old.game_space_id,
        game_id: old.game_id,
        revision: old.revision,
        installations: Vec::new(),
        instances: old
            .instances
            .into_iter()
            .map(|old| ServerInstance {
                schema_version: SERVER_SCHEMA_VERSION,
                id: old.id,
                game_space_id: old.game_space_id,
                revision: old.revision,
                display_name: old.display_name,
                installation_id: None,
                definition: old.definition,
                configuration_import: None,
            })
            .collect(),
    })
}

pub(super) fn instance(bytes: &[u8]) -> Result<ServerInstance, ServerStoreError> {
    let old: InstanceV1 = serde_json::from_slice(bytes).map_err(|_| ServerStoreError::Corrupt)?;
    if old.schema_version != 1 {
        return Err(ServerStoreError::UnsupportedVersion);
    }
    Ok(ServerInstance {
        schema_version: SERVER_SCHEMA_VERSION,
        id: old.id,
        game_space_id: old.game_space_id,
        revision: old.revision,
        display_name: old.display_name,
        installation_id: None,
        definition: old.definition,
        configuration_import: None,
    })
}
