#![cfg_attr(not(test), allow(dead_code))]

use super::definition::scenario_resource_valid;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ScenarioSelection {
    pub resource: String,
    pub revision: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ScenarioError {
    Resource,
    Revision,
}

pub(crate) fn installation_was_probed() -> bool {
    false
}

pub(crate) fn select_scenario(
    resource: &str,
    revision: u64,
) -> Result<ScenarioSelection, ScenarioError> {
    if revision == 0 {
        return Err(ScenarioError::Revision);
    }
    if !scenario_resource_valid(resource) {
        return Err(ScenarioError::Resource);
    }
    Ok(ScenarioSelection {
        resource: resource.to_string(),
        revision,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_scenario_selection_does_not_probe_an_installation() {
        let selected = select_scenario("{0123456789ABCDEF}missions/main.conf", 3).unwrap();
        assert_eq!(selected.revision, 3);
        assert!(!installation_was_probed());
        assert_eq!(
            select_scenario("../secret.conf", 3),
            Err(ScenarioError::Resource)
        );
        assert_eq!(
            select_scenario("{0123456789ABCDEF}missions/main.conf", 0),
            Err(ScenarioError::Revision)
        );
    }
}
