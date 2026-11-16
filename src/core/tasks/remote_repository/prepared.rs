use super::{
    FoxyContext, FoxyMode, FoxyRepository, HashAlgorithmPreference, fetch_json_fresh,
    repository_addon_paths_match_space_layout,
};
use crate::core::db::params;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;

#[derive(PartialEq, Eq)]
struct PreparedAddonVersion {
    checksum: String,
    required: bool,
    client_side: bool,
    data_order: i64,
}

fn published_addons(data: &Value) -> Option<HashMap<String, PreparedAddonVersion>> {
    let mut addons = HashMap::new();
    let mut saw_list = false;
    for (key, required) in [("requiredMods", true), ("optionalMods", false)] {
        let Some(list) = data.get(key) else {
            continue;
        };
        saw_list = true;
        for (data_order, addon) in list.as_array()?.iter().enumerate() {
            let name = addon.get("modName")?.as_str()?.trim();
            let checksum = addon
                .get("checkSum")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .trim();
            if name.is_empty() || !crate::core::utils::fs_safety::is_safe_child_path(name) {
                return None;
            }
            let previous = addons.insert(
                name.to_lowercase(),
                PreparedAddonVersion {
                    checksum: checksum.to_ascii_uppercase(),
                    required,
                    client_side: addon
                        .get("clientSide")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                    data_order: data_order as i64,
                },
            );
            if previous.is_some() {
                return None;
            }
        }
    }
    saw_list.then_some(addons)
}

async fn stored_addons(
    context: &FoxyContext,
    repository_id: u64,
) -> Option<HashMap<String, PreparedAddonVersion>> {
    let rows = context
        .db()
        .query_all(
            "SELECT a.name, a.remote_checksum, a.required, a.client_side, a.data_order \
         FROM repository_addons ra JOIN addons a ON a.id = ra.addon_id \
         WHERE ra.repository_id = ?",
            params![repository_id as i64],
        )
        .await
        .ok()?;
    let mut addons = HashMap::new();
    for row in rows {
        let name = row.get_string("name").ok()?.trim().to_lowercase();
        let version = PreparedAddonVersion {
            checksum: row
                .get_string("remote_checksum")
                .ok()?
                .trim()
                .to_ascii_uppercase(),
            required: row.get_bool("required").ok()?,
            client_side: row.get_bool("client_side").ok()?,
            data_order: row.get_i64("data_order").ok()?,
        };
        if addons.insert(name, version).is_some() {
            return None;
        }
    }
    Some(addons)
}

pub(super) async fn addon_properties_match(
    context: &FoxyContext,
    repository_id: u64,
    data: &Value,
) -> Option<bool> {
    let remote = published_addons(data)?;
    let stored = stored_addons(context, repository_id).await?;
    Some(
        remote.len() == stored.len()
            && remote.iter().all(|(name, remote)| {
                stored.get(name).is_some_and(|stored| {
                    remote.required == stored.required
                        && remote.client_side == stored.client_side
                        && remote.data_order == stored.data_order
                })
            }),
    )
}

pub(crate) async fn probe_prepared_repository_checksum(
    context: Arc<FoxyContext>,
    repository: &FoxyRepository,
    hash_algorithm_preference: HashAlgorithmPreference,
) -> Option<String> {
    let normalized_url =
        crate::core::models::repository::normalize_repository_url(&repository.remote_url);
    let mut data = fetch_json_fresh(context.clone(), &format!("{normalized_url}repo.json"))
        .await
        .ok()?;
    let remote_foxy_mode = FoxyMode::from_db_str(
        data.get("foxyMode")
            .and_then(Value::as_str)
            .unwrap_or_default(),
    );
    let foxy_mode = if hash_algorithm_preference == HashAlgorithmPreference::PreferSwifty {
        FoxyMode::None
    } else {
        remote_foxy_mode
    };
    if foxy_mode != repository.foxy_mode {
        return None;
    }
    if foxy_mode.is_foxy() {
        data = fetch_json_fresh(
            context.clone(),
            &format!("{normalized_url}foxy_addons.json"),
        )
        .await
        .ok()?;
    }
    let checksum = data.get("checksum")?.as_str()?.trim();
    if checksum.is_empty() {
        return None;
    }

    let remote = published_addons(&data)?;
    let stored = stored_addons(&context, repository.id).await?;
    if remote.values().any(|version| version.checksum.is_empty())
        || remote != stored
        || !repository_addon_paths_match_space_layout(context, repository).await
    {
        log::info!("Prepared download metadata changed; rebuilding the update plan");
        return None;
    }
    Some(checksum.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn published_addon_versions_preserve_category_order_and_client_side() {
        let versions = published_addons(&json!({
            "requiredMods": [
                {"modName": "@mod_01", "checkSum": " aBc "},
                {"modName": "@mod_02", "checkSum": "DEF", "clientSide": true}
            ],
            "optionalMods": [{"modName": "@mod_03", "checkSum": "GHI"}]
        }))
        .unwrap();
        assert_eq!(versions["@mod_01"].checksum, "ABC");
        assert!(versions["@mod_01"].required);
        assert_eq!(versions["@mod_02"].data_order, 1);
        assert!(versions["@mod_02"].client_side);
        assert!(!versions["@mod_03"].required);
        assert_eq!(versions["@mod_03"].data_order, 0);
        assert!(
            published_addons(&json!({"requiredMods": []}))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn published_addon_versions_reject_ambiguous_or_invalid_lists() {
        for data in [
            json!({}),
            json!({"requiredMods": {}}),
            json!({"requiredMods": [{"checkSum": "ABC"}]}),
            json!({"requiredMods": [{"modName": "../@mod_01", "checkSum": "ABC"}]}),
            json!({
                "requiredMods": [{"modName": "@mod_01", "checkSum": "ABC"}],
                "optionalMods": [{"modName": "@MOD_01", "checkSum": "ABC"}]
            }),
        ] {
            assert!(published_addons(&data).is_none());
        }
    }
}
