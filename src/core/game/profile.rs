use serde::{Deserialize, Serialize};

use crate::ui::types::RepositoryProfile;

/// Which backend a referenced mod comes from. String-tagged so the pack format
/// stays stable and readable.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModSourceKind {
    Repository,
    SteamWorkshop,
    ReforgerWorkshop,
    DirectDownload,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModRefKind {
    #[default]
    Required,
    Optional,
    External,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ModRef {
    pub source: ModSourceKind,
    pub name: String,
    #[serde(default)]
    pub kind: ModRefKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Profile {
    pub name: String,
    #[serde(default)]
    pub enabled_mods: Vec<ModRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_folder: Option<String>,
    #[serde(default)]
    pub extra_files: Vec<String>,
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub launch: serde_json::Value,
}

pub fn generic_profile_from_repository_profile(
    profile: &RepositoryProfile,
    repository_url: &str,
) -> Profile {
    let url = (!repository_url.trim().is_empty()).then(|| repository_url.trim().to_string());
    let mut enabled_mods = Vec::new();
    for (name, enabled) in &profile.addons {
        if *enabled {
            enabled_mods.push(ModRef {
                source: ModSourceKind::Repository,
                name: name.clone(),
                kind: ModRefKind::Required,
                repository_url: url.clone(),
                path: None,
            });
        }
    }
    for (name, enabled) in &profile.optional_addons {
        if *enabled {
            enabled_mods.push(ModRef {
                source: ModSourceKind::Repository,
                name: name.clone(),
                kind: ModRefKind::Optional,
                repository_url: url.clone(),
                path: None,
            });
        }
    }
    for (name, enabled, path) in &profile.external_addons {
        if *enabled {
            enabled_mods.push(ModRef {
                source: ModSourceKind::Repository,
                name: name.clone(),
                kind: ModRefKind::External,
                repository_url: url.clone(),
                path: (!path.trim().is_empty()).then(|| path.trim().to_string()),
            });
        }
    }
    Profile {
        name: profile.name.clone(),
        enabled_mods,
        config_folder: None,
        extra_files: Vec::new(),
        launch: serde_json::Value::Null,
    }
}

pub fn generic_repository_profile_from_profile(profile: &Profile) -> RepositoryProfile {
    let mut repository_profile = RepositoryProfile {
        name: profile.name.clone(),
        ..RepositoryProfile::default()
    };
    for mod_ref in &profile.enabled_mods {
        if mod_ref.source != ModSourceKind::Repository {
            continue;
        }
        match mod_ref.kind {
            ModRefKind::Required => {
                repository_profile.addons.push((mod_ref.name.clone(), true));
            }
            ModRefKind::Optional => {
                repository_profile
                    .optional_addons
                    .push((mod_ref.name.clone(), true));
            }
            ModRefKind::External => {
                repository_profile.external_addons.push((
                    mod_ref.name.clone(),
                    true,
                    mod_ref.path.clone().unwrap_or_default(),
                ));
            }
        }
    }
    repository_profile
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_repository_profile() -> RepositoryProfile {
        RepositoryProfile {
            name: "Event".to_string(),
            addons: vec![("@core".to_string(), true), ("@maps".to_string(), false)],
            optional_addons: vec![("@blastcore".to_string(), true)],
            external_addons: vec![("@client".to_string(), true, "D:/mods".to_string())],
            ..RepositoryProfile::default()
        }
    }

    #[test]
    fn generic_mapping_collects_enabled_mods_only() {
        let profile = generic_profile_from_repository_profile(
            &sample_repository_profile(),
            "https://repo.example/main/",
        );

        assert_eq!(profile.name, "Event");
        assert_eq!(profile.enabled_mods.len(), 3);
        assert!(profile.launch.is_null());
        let names: Vec<&str> = profile
            .enabled_mods
            .iter()
            .map(|m| m.name.as_str())
            .collect();
        assert_eq!(names, vec!["@core", "@blastcore", "@client"]);
        assert!(
            profile
                .enabled_mods
                .iter()
                .all(|m| m.repository_url.as_deref() == Some("https://repo.example/main/"))
        );
        assert_eq!(profile.enabled_mods[2].kind, ModRefKind::External);
        assert_eq!(profile.enabled_mods[2].path.as_deref(), Some("D:/mods"));
    }

    #[test]
    fn generic_reverse_mapping_restores_enable_state() {
        let generic = generic_profile_from_repository_profile(
            &sample_repository_profile(),
            "https://repo.example/main/",
        );

        let restored = generic_repository_profile_from_profile(&generic);

        assert_eq!(restored.name, "Event");
        assert_eq!(restored.addons, vec![("@core".to_string(), true)]);
        assert_eq!(
            restored.optional_addons,
            vec![("@blastcore".to_string(), true)]
        );
        assert_eq!(
            restored.external_addons,
            vec![("@client".to_string(), true, "D:/mods".to_string())]
        );
    }
}
