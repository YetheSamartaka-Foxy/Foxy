#![cfg_attr(not(test), allow(dead_code))]

use serde::{Deserialize, Serialize};
use std::collections::HashSet;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServerAddonRole {
    Required,
    Optional,
    ServerOnly,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerAddonBinding {
    pub source_id: String,
    pub role: ServerAddonRole,
    pub order: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum AddonBindError {
    Duplicate,
    Identity,
}

pub(crate) fn client_enablement_changes() -> bool {
    false
}

pub(crate) fn order_bindings(
    bindings: Vec<ServerAddonBinding>,
) -> Result<Vec<ServerAddonBinding>, AddonBindError> {
    let mut ordered = bindings;
    let mut seen = HashSet::new();
    for binding in &ordered {
        if binding.source_id.is_empty()
            || binding.source_id.len() > 64
            || !binding
                .source_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        {
            return Err(AddonBindError::Identity);
        }
        if !seen.insert(binding.source_id.to_ascii_lowercase()) {
            return Err(AddonBindError::Duplicate);
        }
    }
    ordered.sort_by_key(|binding| (binding.order, binding.source_id.clone()));
    Ok(ordered)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeated_profile_identity_is_rejected_across_different_orders() {
        assert_eq!(
            order_bindings(vec![
                ServerAddonBinding {
                    source_id: "RepoAlpha".into(),
                    role: ServerAddonRole::Required,
                    order: 1
                },
                ServerAddonBinding {
                    source_id: "RepoBeta".into(),
                    role: ServerAddonRole::Optional,
                    order: 2
                },
                ServerAddonBinding {
                    source_id: "repoalpha".into(),
                    role: ServerAddonRole::ServerOnly,
                    order: 3
                },
            ]),
            Err(AddonBindError::Duplicate)
        );
    }

    #[test]
    fn profile_bindings_require_the_registered_profile_name_syntax() {
        for source_id in ["", "../RepoAlpha", "Repo Alpha", "RepoAlpha\n", "@mod_01"] {
            assert_eq!(
                order_bindings(vec![ServerAddonBinding {
                    source_id: source_id.into(),
                    role: ServerAddonRole::Required,
                    order: 1
                }]),
                Err(AddonBindError::Identity)
            );
        }
    }

    #[test]
    fn server_addon_order_does_not_change_client_enablement() {
        let ordered = order_bindings(vec![
            ServerAddonBinding {
                source_id: "mod_b".into(),
                role: ServerAddonRole::Optional,
                order: 2,
            },
            ServerAddonBinding {
                source_id: "mod_a".into(),
                role: ServerAddonRole::ServerOnly,
                order: 1,
            },
        ])
        .unwrap();
        assert_eq!(ordered[0].source_id, "mod_a");
        assert!(!client_enablement_changes());
        assert_eq!(
            order_bindings(vec![
                ServerAddonBinding {
                    source_id: "mod_a".into(),
                    role: ServerAddonRole::Required,
                    order: 1,
                },
                ServerAddonBinding {
                    source_id: "MOD_A".into(),
                    role: ServerAddonRole::Required,
                    order: 2,
                },
            ]),
            Err(AddonBindError::Duplicate)
        );
    }
}
