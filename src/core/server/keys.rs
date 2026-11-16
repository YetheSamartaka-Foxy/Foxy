mod format;
mod review;
pub(in crate::core::server) mod roles;
pub(in crate::core::server) mod runtime;

use super::{ServerContext, ServerInstanceId};
pub use format::PublicKeyIdentity;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

pub(in crate::core::server) const BASE_KEY_HASH: &str =
    "8530ec416f906b7df3d2d1106d1cee6224090a9fee1dfb1f872644931328dca4";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ServerKeyOrigin {
    Base {
        installation_id: String,
        path: String,
    },
    Stage {
        source_id: String,
        path: String,
    },
    Operator {
        filename: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrustedServerKey {
    pub name: String,
    pub identity: PublicKeyIdentity,
    pub blake3: String,
    pub material: Vec<u8>,
    pub origins: Vec<ServerKeyOrigin>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerKeyTrust {
    pub instance_id: ServerInstanceId,
    pub keys: Vec<TrustedServerKey>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ServerKeyCandidate {
    pub id: String,
    pub key: TrustedServerKey,
    pub trusted: bool,
    pub effective_base: bool,
    pub implicit_tested_base: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct ServerKeyReview {
    #[serde(skip)]
    context: ServerContext,
    #[serde(skip)]
    operator_files: Vec<PathBuf>,
    pub instance_id: ServerInstanceId,
    pub instance_revision: u64,
    pub stage_id: Option<String>,
    pub candidates: Vec<ServerKeyCandidate>,
    pub issues: Vec<String>,
    pub fingerprint: String,
    pub signature_verification: String,
    pub cryptographic_verification: bool,
    pub writes_performed: bool,
}

pub(super) fn key_name_valid(value: &str) -> bool {
    value.len() <= 128
        && value.to_ascii_lowercase().ends_with(".bikey")
        && value.len() > 6
        && !value.contains("..")
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        && !matches!(
            value
                .split('.')
                .next()
                .unwrap_or_default()
                .to_ascii_uppercase()
                .as_str(),
            "CON"
                | "PRN"
                | "AUX"
                | "NUL"
                | "COM1"
                | "COM2"
                | "COM3"
                | "COM4"
                | "COM5"
                | "COM6"
                | "COM7"
                | "COM8"
                | "COM9"
                | "LPT1"
                | "LPT2"
                | "LPT3"
                | "LPT4"
                | "LPT5"
                | "LPT6"
                | "LPT7"
                | "LPT8"
                | "LPT9"
        )
}

impl TrustedServerKey {
    pub(super) fn new(
        name: String,
        material: Vec<u8>,
        origin: ServerKeyOrigin,
    ) -> Result<Self, String> {
        if !key_name_valid(&name) {
            return Err("Invalid public-key filename".into());
        }
        let identity = format::inspect(&material)?;
        Ok(Self {
            name,
            identity,
            blake3: blake3::hash(&material).to_hex().to_string(),
            material,
            origins: vec![origin],
        })
    }

    pub(super) fn validate(&self) -> Result<(), String> {
        if !key_name_valid(&self.name)
            || format::inspect(&self.material)? != self.identity
            || blake3::hash(&self.material).to_hex().as_str() != self.blake3
            || self.origins.is_empty()
            || self.origins.len() > 128
        {
            return Err("Public-key identity or material is invalid".into());
        }
        for origin in &self.origins {
            match origin {
                ServerKeyOrigin::Base {
                    installation_id,
                    path,
                } => {
                    if installation_id.len() > 128
                        || installation_id.is_empty()
                        || path.len() > 1024
                    {
                        return Err("Invalid base-key provenance".into());
                    }
                    super::content_staging::parse_relative(path)
                        .map_err(|_| "Invalid base-key path")?;
                }
                ServerKeyOrigin::Stage { source_id, path } => {
                    if source_id.is_empty() || source_id.len() > 64 || path.len() > 1024 {
                        return Err("Invalid staged-key provenance".into());
                    }
                    super::content_staging::parse_relative(path)
                        .map_err(|_| "Invalid staged-key path")?;
                }
                ServerKeyOrigin::Operator { filename } if !key_name_valid(filename) => {
                    return Err("Invalid operator-key provenance".into());
                }
                ServerKeyOrigin::Operator { .. } => {}
            }
        }
        Ok(())
    }
}

impl ServerKeyTrust {
    pub(crate) fn validate(&self) -> Result<(), String> {
        if self.keys.len() > 128
            || self
                .keys
                .iter()
                .map(|key| key.material.len())
                .sum::<usize>()
                > 512 * 1024
        {
            return Err("Public-key trust policy exceeds its size limit".into());
        }
        let mut names = BTreeSet::new();
        let mut authorities = BTreeMap::new();
        for key in &self.keys {
            key.validate()?;
            if !names.insert(key.name.to_ascii_lowercase()) {
                return Err("Duplicate public-key destination".into());
            }
            if let Some(before) = authorities.insert(
                key.identity.authority.to_ascii_lowercase(),
                &key.identity.fingerprint,
            ) && before != &key.identity.fingerprint
            {
                return Err("Different public keys use the same authority".into());
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn committed_key_trust_example_is_a_valid_policy_for_its_own_instance() {
        let instance: crate::core::server::ServerInstance = serde_json::from_str(include_str!(
            "../../../examples/json/server_backend/arma3_key_trust_instance.json"
        ))
        .unwrap();
        instance
            .validate("arma3", crate::core::server::ServerDefinitionKind::Arma3)
            .unwrap();
        let crate::core::server::ServerDefinition::Arma3(settings) = &instance.definition else {
            panic!("example must be an Arma instance");
        };
        let trust = settings.key_trust.as_deref().unwrap();
        assert_eq!(trust.instance_id, instance.id);
        assert_eq!(trust.keys.len(), 1);
        assert_eq!(trust.keys[0].identity.bits, 1024);
        assert!(matches!(
            trust.keys[0].origins.as_slice(),
            [ServerKeyOrigin::Stage { source_id, .. }] if source_id == "RepoAlpha"
        ));
    }

    #[test]
    fn public_key_destinations_reject_traversal_and_windows_aliases() {
        assert!(key_name_valid("Example_01.bikey"));
        for name in [
            "../Example.bikey",
            "NUL.bikey",
            "example.biprivatekey",
            "a:b.bikey",
            "a.bikey.",
            "a/b.bikey",
        ] {
            assert!(!key_name_valid(name), "{name}");
        }
    }
}
