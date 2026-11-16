use std::collections::{BTreeMap, BTreeSet};

use super::super::ReforgerAddonRequirement;
use super::model::{
    ReforgerAddonChannel, ReforgerAddonLock, ReforgerAddonMetadata, ReforgerAddonOmission,
};

const MAX_NODES: usize = 256;
const MAX_EDGES: usize = 2048;
const MAX_INPUTS: usize = 4096;

fn valid_version(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
}

fn guid(value: &str) -> Result<String, String> {
    let normalized = value.to_ascii_lowercase();
    if normalized.len() != 16 || !normalized.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("Invalid Reforger addon GUID".into());
    }
    Ok(normalized)
}

pub fn resolve(
    requirements: &[ReforgerAddonRequirement],
    default_required: Option<bool>,
    scenario_id: &str,
    scenario_provider: Option<&str>,
    channel: ReforgerAddonChannel,
    metadata: &[ReforgerAddonMetadata],
) -> Result<ReforgerAddonLock, String> {
    if requirements.len() > MAX_INPUTS || metadata.len() > MAX_INPUTS {
        return Err("Reforger addon input exceeds its item limit".into());
    }
    if !super::super::definition::scenario_resource_valid(scenario_id) {
        return Err("Invalid Reforger scenario resource".into());
    }
    let mut observed = BTreeMap::new();
    for item in metadata {
        if item.dependencies.len() > MAX_EDGES || item.scenarios.len() > MAX_NODES {
            return Err("Reforger addon metadata exceeds its item limit".into());
        }
        let id = guid(&item.addon_id)?;
        if !valid_version(&item.version) {
            return Err("Invalid Reforger addon version metadata".into());
        }
        let mut normalized = item.clone();
        normalized.addon_id = id.clone();
        for dependency in &mut normalized.dependencies {
            dependency.addon_id = guid(&dependency.addon_id)?;
            if dependency
                .version
                .as_deref()
                .is_some_and(|version| !valid_version(version))
            {
                return Err("Invalid Reforger addon dependency version".into());
            }
        }
        if normalized
            .scenarios
            .iter()
            .any(|scenario| !super::super::definition::scenario_resource_valid(scenario))
        {
            return Err("Invalid scenario resource in addon metadata".into());
        }
        if observed.insert(id.clone(), normalized).is_some() {
            return Err(format!("Duplicate metadata for addon {id}"));
        }
    }

    let mut requested = Vec::with_capacity(requirements.len());
    let mut roots = BTreeMap::<String, (bool, Option<String>)>::new();
    let mut root_order = Vec::with_capacity(requirements.len());
    for item in requirements {
        let id = guid(&item.addon_id)?;
        if item
            .version
            .as_deref()
            .is_some_and(|version| !valid_version(version))
        {
            return Err("Invalid exact addon version constraint".into());
        }
        let required = item.required.unwrap_or(default_required.unwrap_or(true));
        if roots.contains_key(&id) {
            return Err("Duplicate addon requirement".into());
        }
        roots.insert(id.clone(), (required, item.version.clone()));
        root_order.push(id.clone());
        let mut copy = item.clone();
        copy.addon_id = id;
        requested.push(copy);
    }

    let mut scenario_guid = None;
    if let Some(provider) = scenario_provider {
        scenario_guid = Some(guid(provider)?);
    }
    let mut realized_ids = BTreeSet::new();
    let mut realized_order = Vec::new();
    let mut required_ids = BTreeSet::new();
    let mut omissions = Vec::new();
    let mut pending = Vec::<(String, Option<String>, bool, usize)>::new();
    for (id, (required, version)) in &roots {
        pending.push((id.clone(), version.clone(), *required, 0));
    }
    if let Some(id) = &scenario_guid {
        pending.push((id.clone(), None, true, 0));
    }
    let mut edge_count = 0;
    let mut realized = BTreeMap::new();
    while let Some((id, pin, is_required, depth)) = pending.pop() {
        if depth > MAX_NODES {
            return Err("Reforger addon dependency graph exceeds its node/depth limit".into());
        }
        if is_required {
            required_ids.insert(id.clone());
        }
        if let (Some(pin), Some(item)) = (pin.as_deref(), observed.get(&id))
            && pin != item.version
        {
            return Err("Addon does not match its exact version constraint".into());
        }
        if realized_ids.contains(&id) {
            continue;
        }
        if realized_ids.len() >= MAX_NODES {
            return Err("Reforger addon dependency graph exceeds its node/depth limit".into());
        }
        let Some(item) = observed.get(&id) else {
            if is_required {
                return Err(format!("Required addon metadata is unavailable for {id}"));
            }
            omissions.push(ReforgerAddonOmission {
                addon_id: id,
                reason: "metadata unavailable".into(),
            });
            continue;
        };
        if item.channel != channel {
            return Err(format!("Addon {id} is from a different channel"));
        }
        realized_ids.insert(id.clone());
        realized_order.push(id.clone());
        realized.insert(id.clone(), item.clone());
        let mut deps = item
            .dependencies
            .iter()
            .map(|dep| (dep.addon_id.clone(), dep.version.clone()))
            .collect::<Vec<_>>();
        deps.sort();
        deps.dedup();
        for (dep_id, constraint) in deps {
            edge_count += 1;
            if edge_count > MAX_EDGES {
                return Err("Reforger addon dependency graph exceeds its edge limit".into());
            }
            pending.push((dep_id, constraint, true, depth + 1));
        }
    }

    let mut edges = BTreeMap::<String, Vec<(String, Option<String>)>>::new();
    for (id, item) in &realized {
        let mut deps = item
            .dependencies
            .iter()
            .map(|dep| (dep.addon_id.clone(), dep.version.clone()))
            .collect::<Vec<_>>();
        deps.sort();
        for (dep_id, version) in deps {
            if let Some(existing) = realized.get(&dep_id) {
                if version.as_deref().is_some_and(|v| v != existing.version) {
                    return Err("Dependency violates an exact version constraint".into());
                }
                edges.entry(id.clone()).or_default().push((dep_id, version));
            }
        }
    }
    fn visit(
        id: &str,
        edges: &BTreeMap<String, Vec<(String, Option<String>)>>,
        active: &mut BTreeSet<String>,
        done: &mut BTreeSet<String>,
    ) -> Result<(), String> {
        if active.contains(id) {
            return Err(format!("Reforger addon dependency cycle at {id}"));
        }
        if !done.insert(id.to_owned()) {
            return Ok(());
        }
        active.insert(id.to_owned());
        for (next, _) in edges.get(id).into_iter().flatten() {
            visit(next, edges, active, done)?;
        }
        active.remove(id);
        Ok(())
    }
    let mut active = BTreeSet::new();
    let mut done = BTreeSet::new();
    for id in realized.keys() {
        visit(id, &edges, &mut active, &mut done)?;
    }

    if let Some(provider) = &scenario_guid {
        let item = realized
            .get(provider)
            .ok_or_else(|| "Scenario provider was not realized".to_owned())?;
        if !item
            .scenarios
            .iter()
            .any(|scenario| scenario == scenario_id)
        {
            return Err("Selected scenario is not owned by the selected provider".into());
        }
    }
    for (id, (required, _)) in &roots {
        if !required && !realized_ids.contains(id) && required_ids.contains(id) {
            return Err("Optional addon is required by another selected addon".into());
        }
    }
    for (id, (required, _)) in &roots {
        if !required && !realized_ids.contains(id) && !omissions.iter().any(|o| o.addon_id == *id) {
            omissions.push(ReforgerAddonOmission {
                addon_id: id.clone(),
                reason: "optional addon unavailable".into(),
            });
        }
    }
    omissions.sort_by(|a, b| a.addon_id.cmp(&b.addon_id));
    omissions.dedup_by(|a, b| a.addon_id == b.addon_id);
    let mut order = root_order;
    order.extend(realized_order);
    let mut emitted = BTreeSet::new();
    let realized: Vec<_> = order
        .into_iter()
        .filter(|id| emitted.insert(id.clone()))
        .filter_map(|id| realized.remove(&id))
        .collect();
    Ok(ReforgerAddonLock {
        channel,
        requested,
        mods_required_by_default: default_required,
        scenario_id: scenario_id.to_owned(),
        scenario_provider: scenario_guid,
        realized,
        degraded: !omissions.is_empty(),
        omissions,
        content_sources: BTreeMap::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::super::model::ReforgerAddonDependency;
    use super::*;
    use std::collections::BTreeMap;

    fn req(id: &str, version: Option<&str>, required: Option<bool>) -> ReforgerAddonRequirement {
        ReforgerAddonRequirement {
            addon_id: id.into(),
            version: version.map(str::to_owned),
            required,
            preserved: BTreeMap::new(),
        }
    }
    fn meta(
        id: &str,
        version: &str,
        dependencies: Vec<ReforgerAddonDependency>,
    ) -> ReforgerAddonMetadata {
        ReforgerAddonMetadata {
            addon_id: id.into(),
            version: version.into(),
            channel: ReforgerAddonChannel::Stable,
            provider: super::super::model::ReforgerAddonProvider::Local,
            dependencies,
            scenarios: Vec::new(),
        }
    }
    fn dep(id: &str, version: Option<&str>) -> ReforgerAddonDependency {
        ReforgerAddonDependency {
            addon_id: id.into(),
            version: version.map(str::to_owned),
        }
    }

    #[test]
    fn required_default_and_optional_missing_intent_are_preserved() {
        let lock = resolve(
            &[
                req("AAAAAAAAAAAAAAAA", None, None),
                req("bbbbbbbbbbbbbbbb", None, Some(false)),
            ],
            Some(false),
            "{0123456789ABCDEF}missions/main.conf",
            None,
            ReforgerAddonChannel::Stable,
            &[meta("aaaaaaaaaaaaaaaa", "1", Vec::new())],
        )
        .unwrap();
        assert_eq!(lock.requested.len(), 2);
        assert_eq!(lock.realized.len(), 1);
        assert_eq!(lock.omissions[0].addon_id, "bbbbbbbbbbbbbbbb");
        assert!(lock.degraded);
    }

    #[test]
    fn rejects_pin_conflict_duplicate_case_and_dependency_cycle() {
        let scenario = "{0123456789ABCDEF}missions/main.conf";
        assert!(
            resolve(
                &[req("aaaaaaaaaaaaaaaa", Some("1"), Some(true))],
                Some(true),
                scenario,
                None,
                ReforgerAddonChannel::Stable,
                &[meta("aaaaaaaaaaaaaaaa", "2", Vec::new())]
            )
            .is_err()
        );
        assert!(
            resolve(
                &[],
                None,
                scenario,
                None,
                ReforgerAddonChannel::Stable,
                &[
                    meta("aaaaaaaaaaaaaaaa", "1", Vec::new()),
                    meta("AAAAAAAAAAAAAAAA", "1", Vec::new())
                ]
            )
            .is_err()
        );
        assert!(
            resolve(
                &[req("aaaaaaaaaaaaaaaa", None, Some(true))],
                None,
                scenario,
                None,
                ReforgerAddonChannel::Stable,
                &[
                    meta("aaaaaaaaaaaaaaaa", "1", vec![dep("bbbbbbbbbbbbbbbb", None)]),
                    meta("bbbbbbbbbbbbbbbb", "1", vec![dep("aaaaaaaaaaaaaaaa", None)])
                ]
            )
            .is_err()
        );
    }

    #[test]
    fn dependency_cannot_be_omitted_and_scenario_provider_is_separate_from_scenario_id() {
        let required = req("aaaaaaaaaaaaaaaa", None, Some(true));
        assert!(
            resolve(
                &[required, req("bbbbbbbbbbbbbbbb", None, Some(false))],
                None,
                "{0123456789ABCDEF}missions/main.conf",
                None,
                ReforgerAddonChannel::Stable,
                &[meta(
                    "aaaaaaaaaaaaaaaa",
                    "1",
                    vec![dep("bbbbbbbbbbbbbbbb", None)]
                )]
            )
            .is_err()
        );
        let scenario = "{0123456789ABCDEF}missions/main.conf";
        let mut provider = meta("cccccccccccccccc", "1", Vec::new());
        provider.scenarios.push(scenario.into());
        let lock = resolve(
            &[],
            None,
            scenario,
            Some("cccccccccccccccc"),
            ReforgerAddonChannel::Stable,
            &[provider],
        )
        .unwrap();
        assert_eq!(lock.scenario_id, scenario);
        assert_eq!(lock.scenario_provider.as_deref(), Some("cccccccccccccccc"));

        let mut wrong_provider = meta("dddddddddddddddd", "1", Vec::new());
        wrong_provider
            .scenarios
            .push("{0123456789ABCDEF}missions/other.conf".into());
        assert!(
            resolve(
                &[],
                None,
                scenario,
                Some("dddddddddddddddd"),
                ReforgerAddonChannel::Stable,
                &[wrong_provider]
            )
            .is_err()
        );
    }

    #[test]
    fn checks_root_pin_even_when_dependency_realized_it_first_and_bounds_inputs() {
        let roots = [
            req("aaaaaaaaaaaaaaaa", None, Some(true)),
            req("bbbbbbbbbbbbbbbb", Some("2"), Some(true)),
        ];
        let metadata = [
            meta("aaaaaaaaaaaaaaaa", "1", vec![dep("bbbbbbbbbbbbbbbb", None)]),
            meta("bbbbbbbbbbbbbbbb", "1", Vec::new()),
        ];
        assert!(
            resolve(
                &roots,
                None,
                "{0123456789ABCDEF}missions/main.conf",
                None,
                ReforgerAddonChannel::Stable,
                &metadata
            )
            .is_err()
        );
        let many = vec![req("aaaaaaaaaaaaaaaa", None, Some(false)); MAX_INPUTS + 1];
        assert!(
            resolve(
                &many,
                None,
                "{0123456789ABCDEF}missions/main.conf",
                None,
                ReforgerAddonChannel::Stable,
                &[]
            )
            .is_err()
        );
    }
}
