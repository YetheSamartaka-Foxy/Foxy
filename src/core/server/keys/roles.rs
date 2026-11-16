use std::collections::HashSet;

use super::super::{
    Arma3MissionSource, ContentBindingsDocument, ServerStoreError,
    addons::{ServerAddonRole, order_bindings},
    content_staging::StageSource,
};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(in crate::core::server) struct RoleLayout {
    pub required_mods: Vec<String>,
    pub server_mods: Vec<String>,
    pub optional_sources: Vec<String>,
    pub loaded_destinations: Vec<String>,
}

pub(in crate::core::server) fn route(
    bindings: &ContentBindingsDocument,
    sources: &[StageSource],
    missions: &[Arma3MissionSource],
) -> Result<RoleLayout, ServerStoreError> {
    let ordered = order_bindings(
        bindings
            .bindings
            .iter()
            .map(|reviewed| reviewed.binding.clone())
            .collect(),
    )
    .map_err(|_| ServerStoreError::Corrupt)?;
    let mission_ids: HashSet<&str> = missions
        .iter()
        .filter_map(|mission| match mission {
            Arma3MissionSource::Library { mission_id, .. } => Some(mission_id.as_str()),
            Arma3MissionSource::AddonClass => None,
        })
        .collect();
    let mut used_destinations = HashSet::new();
    for source in sources {
        validate_destination(&source.destination)?;
        if !used_destinations.insert(source.destination.to_ascii_lowercase()) {
            return Err(ServerStoreError::Invalid(
                "Content stage has colliding destinations".into(),
            ));
        }
    }
    let profile_names: HashSet<&str> = bindings
        .bindings
        .iter()
        .map(|binding| binding.profile.name.as_str())
        .collect();
    if sources.iter().any(|source| {
        !profile_names.contains(source.profile_name.as_str())
            && !mission_ids.contains(source.profile_name.as_str())
    }) {
        return Err(ServerStoreError::Invalid(
            "Content stage contains an unknown profile source".into(),
        ));
    }
    let mut matched_profiles = HashSet::new();
    let mut layout = RoleLayout::default();

    for binding in ordered {
        let reviewed = bindings
            .bindings
            .iter()
            .find(|reviewed| reviewed.binding.source_id == binding.source_id)
            .ok_or(ServerStoreError::Corrupt)?;
        let matching: Vec<_> = sources
            .iter()
            .filter(|source| source.profile_name == reviewed.profile.name)
            .collect();
        if matching.is_empty() {
            return Err(ServerStoreError::NotFound);
        }
        matched_profiles.insert(reviewed.profile.name.as_str());
        for source in matching {
            if mission_ids.contains(source.profile_name.as_str()) {
                continue;
            }
            let folder = source
                .destination
                .rsplit('/')
                .next()
                .ok_or(ServerStoreError::UnsafePath)?;
            let content_path = format!("foxy-content/{folder}");
            match binding.role {
                ServerAddonRole::Required => {
                    layout.required_mods.push(content_path);
                    layout.loaded_destinations.push(source.destination.clone());
                }
                ServerAddonRole::ServerOnly => {
                    layout.server_mods.push(content_path);
                    layout.loaded_destinations.push(source.destination.clone());
                }
                ServerAddonRole::Optional => {
                    layout.optional_sources.push(source.destination.clone());
                }
            }
        }
    }

    for source in sources {
        if !matched_profiles.contains(source.profile_name.as_str())
            && !mission_ids.contains(source.profile_name.as_str())
        {
            return Err(ServerStoreError::Invalid(
                "Content stage contains an unknown profile source".into(),
            ));
        }
    }
    Ok(layout)
}

pub(in crate::core::server) fn headless_mods(
    bindings: &ContentBindingsDocument,
    sources: &[StageSource],
    missions: &[Arma3MissionSource],
    client: &crate::core::server::Arma3HeadlessClient,
) -> Result<Vec<String>, ServerStoreError> {
    let ordered = order_bindings(
        bindings
            .bindings
            .iter()
            .map(|reviewed| reviewed.binding.clone())
            .collect(),
    )
    .map_err(|_| ServerStoreError::Corrupt)?;
    let selection = &client.addons;
    for (named, excluded) in selection
        .include
        .iter()
        .map(|id| (id, false))
        .chain(selection.exclude.iter().map(|id| (id, true)))
    {
        let binding = ordered
            .iter()
            .find(|binding| binding.source_id.eq_ignore_ascii_case(named))
            .ok_or_else(|| {
                ServerStoreError::Invalid(format!(
                    "Headless client {} names content source {named}, which is not bound to this server.",
                    client.name
                ))
            })?;
        if !excluded && binding.role == ServerAddonRole::Optional {
            return Err(ServerStoreError::Invalid(format!(
                "Headless client {} selects optional source {named}. Optional sources are not part of the server runtime; make it required or server-only first.",
                client.name
            )));
        }
        if excluded && binding.role != ServerAddonRole::Required {
            return Err(ServerStoreError::Invalid(format!(
                "Headless client {} excludes {named}, but exclusions apply only to required server addons.",
                client.name
            )));
        }
    }
    let mission_ids: HashSet<&str> = missions
        .iter()
        .filter_map(|mission| match mission {
            Arma3MissionSource::Library { mission_id, .. } => Some(mission_id.as_str()),
            Arma3MissionSource::AddonClass => None,
        })
        .collect();
    let mut mods = Vec::new();
    for binding in ordered {
        if binding.role == ServerAddonRole::Optional
            || !crate::core::server::headless::selects(
                selection,
                &binding.source_id,
                binding.role == ServerAddonRole::Required,
            )
        {
            continue;
        }
        let reviewed = bindings
            .bindings
            .iter()
            .find(|reviewed| reviewed.binding.source_id == binding.source_id)
            .ok_or(ServerStoreError::Corrupt)?;
        for source in sources
            .iter()
            .filter(|source| source.profile_name == reviewed.profile.name)
        {
            if mission_ids.contains(source.profile_name.as_str()) {
                continue;
            }
            let folder = source
                .destination
                .rsplit('/')
                .next()
                .ok_or(ServerStoreError::UnsafePath)?;
            mods.push(format!("foxy-content/{folder}"));
        }
    }
    Ok(mods)
}

fn validate_destination(destination: &str) -> Result<(), ServerStoreError> {
    if !destination.starts_with("addons/")
        || destination.contains(['\\', '\0', ':'])
        || destination
            .split('/')
            .any(|component| component.is_empty() || component == "." || component == "..")
    {
        return Err(ServerStoreError::UnsafePath);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::server::{
        ServerAddonBinding, ServerInstanceId,
        content_bindings::ReviewedContentBinding,
        content_sources::{AddonFolder, AddonSupply, RepositoryProfile},
    };
    use std::path::PathBuf;

    fn binding(
        source_id: &str,
        role: ServerAddonRole,
        order: u32,
        profile: &str,
    ) -> ReviewedContentBinding {
        ReviewedContentBinding {
            binding: ServerAddonBinding {
                source_id: source_id.into(),
                role,
                order,
            },
            profile: RepositoryProfile {
                name: profile.into(),
                remote_url: "https://repo.example.invalid/".into(),
                local_path: PathBuf::from("repo"),
                folder_id: "folder".into(),
                supply: AddonSupply::AlreadyPresent,
                addons: Vec::new(),
            },
            folder: AddonFolder {
                id: "folder".into(),
                path: PathBuf::from("folder"),
            },
        }
    }

    fn document(bindings: Vec<ReviewedContentBinding>) -> ContentBindingsDocument {
        ContentBindingsDocument {
            schema_version: 1,
            game_space_id: "space".into(),
            game_id: "arma3".into(),
            instance_id: ServerInstanceId::new(),
            generation: 1,
            instance_revision: 1,
            catalog_revision: 1,
            catalog_fingerprint: "catalog".into(),
            content_sources_fingerprint: None,
            bindings,
        }
    }

    fn source(profile_name: &str, destination: &str) -> StageSource {
        StageSource {
            profile_name: profile_name.into(),
            source: PathBuf::from("source"),
            destination: destination.into(),
        }
    }

    #[test]
    fn routes_in_binding_then_stage_source_order_and_excludes_optional() {
        let bindings = document(vec![
            binding("later", ServerAddonRole::ServerOnly, 2, "Beta"),
            binding("first", ServerAddonRole::Required, 1, "Alpha"),
            binding("optional", ServerAddonRole::Optional, 3, "Gamma"),
        ]);
        let sources = [
            source("Beta", "addons/@mod_03"),
            source("Alpha", "addons/@mod_01"),
            source("Alpha", "addons/@mod_02"),
            source("Gamma", "addons/@mod_04"),
        ];
        let layout = route(&bindings, &sources, &[]).unwrap();
        assert_eq!(
            layout.required_mods,
            ["foxy-content/@mod_01", "foxy-content/@mod_02"]
        );
        assert_eq!(layout.server_mods, ["foxy-content/@mod_03"]);
        assert_eq!(layout.optional_sources, ["addons/@mod_04"]);
        assert_eq!(
            layout.loaded_destinations,
            ["addons/@mod_01", "addons/@mod_02", "addons/@mod_03"]
        );
    }

    #[test]
    fn excludes_explicit_mission_wrappers_from_addon_routing() {
        let bindings = document(vec![binding("repo", ServerAddonRole::Required, 1, "Alpha")]);
        let sources = [
            source("Alpha", "addons/@mod_01"),
            source("mission_abc", "addons/mission_abc"),
        ];
        let missions = [Arma3MissionSource::Library {
            mission_id: "mission_abc".into(),
            revision_id: "revision_abc".into(),
        }];
        let layout = route(&bindings, &sources, &missions).unwrap();
        assert_eq!(layout.loaded_destinations, ["addons/@mod_01"]);
        assert_eq!(layout.required_mods, ["foxy-content/@mod_01"]);
    }

    #[test]
    fn rejects_unknown_profiles_missing_sources_and_case_collisions() {
        let bindings = document(vec![binding("repo", ServerAddonRole::Required, 1, "Alpha")]);
        assert!(matches!(
            route(&bindings, &[source("Other", "addons/@mod_01")], &[]),
            Err(ServerStoreError::Invalid(_))
        ));
        assert!(matches!(
            route(&bindings, &[], &[]),
            Err(ServerStoreError::NotFound)
        ));
        let collision = [
            source("Alpha", "addons/@mod_01"),
            source("Alpha", "addons/@MOD_01"),
        ];
        assert!(matches!(
            route(&bindings, &collision, &[]),
            Err(ServerStoreError::Invalid(_))
        ));
    }

    fn client(
        addons: crate::core::server::HeadlessAddonSelection,
    ) -> crate::core::server::Arma3HeadlessClient {
        crate::core::server::Arma3HeadlessClient {
            id: "hc_0000000000000001".into(),
            name: "HC1".into(),
            order: 0,
            enabled: true,
            addons,
            process: Default::default(),
            lifecycle: Default::default(),
        }
    }

    #[test]
    fn headless_mods_follow_server_order_inherit_required_and_never_load_optional() {
        let bindings = document(vec![
            binding("later", ServerAddonRole::ServerOnly, 2, "Beta"),
            binding("first", ServerAddonRole::Required, 1, "Alpha"),
            binding("optional", ServerAddonRole::Optional, 3, "Gamma"),
            binding("tools", ServerAddonRole::Required, 4, "Delta"),
        ]);
        let sources = [
            source("Beta", "addons/@mod_03"),
            source("Alpha", "addons/@mod_01"),
            source("Alpha", "addons/@mod_02"),
            source("Gamma", "addons/@mod_04"),
            source("Delta", "addons/@mod_05"),
        ];
        let inherited =
            headless_mods(&bindings, &sources, &[], &client(Default::default())).unwrap();
        assert_eq!(
            inherited,
            [
                "foxy-content/@mod_01",
                "foxy-content/@mod_02",
                "foxy-content/@mod_05"
            ]
        );
        let selected = headless_mods(
            &bindings,
            &sources,
            &[],
            &client(crate::core::server::HeadlessAddonSelection {
                inherit_required: true,
                include: vec!["LATER".into()],
                exclude: vec!["tools".into()],
            }),
        )
        .unwrap();
        assert_eq!(
            selected,
            [
                "foxy-content/@mod_01",
                "foxy-content/@mod_02",
                "foxy-content/@mod_03"
            ]
        );
        let explicit = headless_mods(
            &bindings,
            &sources,
            &[],
            &client(crate::core::server::HeadlessAddonSelection {
                inherit_required: false,
                include: vec!["tools".into(), "later".into()],
                exclude: Vec::new(),
            }),
        )
        .unwrap();
        assert_eq!(explicit, ["foxy-content/@mod_03", "foxy-content/@mod_05"]);
        for (include, exclude) in [
            (vec!["optional".to_owned()], Vec::new()),
            (vec!["missing".to_owned()], Vec::new()),
            (Vec::new(), vec!["later".to_owned()]),
        ] {
            assert!(matches!(
                headless_mods(
                    &bindings,
                    &sources,
                    &[],
                    &client(crate::core::server::HeadlessAddonSelection {
                        inherit_required: true,
                        include,
                        exclude,
                    }),
                ),
                Err(ServerStoreError::Invalid(_))
            ));
        }
    }
}
