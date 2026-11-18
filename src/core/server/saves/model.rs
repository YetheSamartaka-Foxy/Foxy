use serde::{Deserialize, Serialize};

use super::super::{ContentStageControl, ServerInstanceId, ServerStore, ServerStoreError};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub enum ReforgerSessionIntent {
    #[default]
    Configured,
    Latest,
    Selected {
        save_id: String,
    },
    New,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ReforgerSessionSummary {
    pub save_id: String,
    pub scenario: String,
    pub game_version: String,
    pub saved_unix: i64,
    pub playthrough: u32,
    pub savepoint: u32,
    pub relative_path: String,
    pub bytes: u64,
    pub blake3: String,
    pub compatible: bool,
    pub blockers: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ReforgerProviderCoverage {
    pub local_default: bool,
    pub blockers: Vec<String>,
}

impl ReforgerProviderCoverage {
    pub(super) fn intent_blockers(&self, intent: &ReforgerSessionIntent) -> Vec<String> {
        if *intent == ReforgerSessionIntent::Configured {
            Vec::new()
        } else {
            self.blockers.clone()
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ReforgerSessionInventory {
    pub instance_id: ServerInstanceId,
    pub generation: u64,
    pub intent: ReforgerSessionIntent,
    pub sessions: Vec<ReforgerSessionSummary>,
    pub provider: ReforgerProviderCoverage,
    pub fingerprint: String,
    pub writes_performed: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct ReforgerSessionPlan {
    #[serde(skip)]
    pub(super) context: super::super::ServerContext,
    pub instance_id: ServerInstanceId,
    pub intent: ReforgerSessionIntent,
    pub expected_generation: u64,
    pub selected_save_id: Option<String>,
    pub inventory_fingerprint: String,
    pub fingerprint: String,
    pub blockers: Vec<String>,
    pub warnings: Vec<String>,
    pub writes_performed: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ReforgerSessionLaunch {
    pub intent: ReforgerSessionIntent,
    pub selected_save_id: Option<String>,
    pub playthrough: Option<u32>,
    pub savepoint: Option<u32>,
    pub load: bool,
    pub keep: bool,
    pub fingerprint: String,
    pub inventory_fingerprint: String,
    pub provider: ReforgerProviderCoverage,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct IntentDocument {
    pub schema_version: u32,
    pub game_space_id: String,
    pub game_id: String,
    pub instance_id: ServerInstanceId,
    pub generation: u64,
    pub intent: ReforgerSessionIntent,
}

impl ServerStore {
    fn intent_path(&self, id: &ServerInstanceId) -> std::path::PathBuf {
        self.record_root(id).join("session-intent.json")
    }

    pub fn plan_reforger_session(
        &self,
        id: &ServerInstanceId,
        intent: ReforgerSessionIntent,
        expected_generation: u64,
        control: &ContentStageControl,
    ) -> Result<ReforgerSessionPlan, ServerStoreError> {
        self.require_stopped_save_writers(id, control)?;
        let inventory = self.reforger_sessions(id, control)?;
        if expected_generation != inventory.generation {
            return Err(ServerStoreError::StaleRevision);
        }
        let mut blockers = inventory.provider.intent_blockers(&intent);
        let selected_save_id = select_session(&inventory, &intent, &mut blockers);
        let fingerprint = digest(&(
            self.context().space_id(),
            self.context().game_id(),
            id,
            expected_generation,
            &intent,
            &inventory.fingerprint,
            &selected_save_id,
        ))?;
        Ok(ReforgerSessionPlan {
            context: self.context().clone(),
            instance_id: id.clone(),
            intent,
            expected_generation,
            selected_save_id,
            inventory_fingerprint: inventory.fingerprint,
            fingerprint,
            blockers,
            warnings: Vec::new(),
            writes_performed: false,
        })
    }

    pub fn apply_reforger_session(
        &self,
        plan: ReforgerSessionPlan,
        control: &ContentStageControl,
    ) -> Result<ReforgerSessionInventory, ServerStoreError> {
        if &plan.context != self.context() {
            return Err(ServerStoreError::StaleRevision);
        }
        if !plan.blockers.is_empty() {
            return Err(ServerStoreError::Invalid(plan.blockers.join(" ")));
        }
        self.require_stopped_save_writers(&plan.instance_id, control)?;
        let _pin = crate::core::server::owner::pin_write(&self.context().database_path())
            .map_err(|_| ServerStoreError::Ownership)?;
        self.require_stopped_save_writers(&plan.instance_id, control)?;
        control.checkpoint()?;
        let current = self.reforger_sessions(&plan.instance_id, control)?;
        let provider_blockers = current.provider.intent_blockers(&plan.intent);
        if !provider_blockers.is_empty() {
            return Err(ServerStoreError::Invalid(provider_blockers.join(" ")));
        }
        let expected_fingerprint = digest(&(
            self.context().space_id(),
            self.context().game_id(),
            &plan.instance_id,
            plan.expected_generation,
            &plan.intent,
            &plan.inventory_fingerprint,
            &plan.selected_save_id,
        ))?;
        if current.generation != plan.expected_generation
            || current.fingerprint != plan.inventory_fingerprint
            || expected_fingerprint != plan.fingerprint
        {
            return Err(ServerStoreError::StaleRevision);
        }
        let mut blockers = Vec::new();
        let selected = select_session(&current, &plan.intent, &mut blockers);
        if !blockers.is_empty() || selected != plan.selected_save_id {
            return Err(ServerStoreError::StaleRevision);
        }
        let document = IntentDocument {
            schema_version: 1,
            game_space_id: self.context().space_id().into(),
            game_id: self.context().game_id().into(),
            instance_id: plan.instance_id.clone(),
            generation: current
                .generation
                .checked_add(1)
                .ok_or(ServerStoreError::Corrupt)?,
            intent: plan.intent,
        };
        write_document(&self.intent_path(&plan.instance_id), &document, control)?;
        let mut inventory =
            self.reforger_sessions(&plan.instance_id, &ContentStageControl::default())?;
        inventory.writes_performed = true;
        Ok(inventory)
    }
}

pub(super) fn select_session(
    inventory: &ReforgerSessionInventory,
    intent: &ReforgerSessionIntent,
    blockers: &mut Vec<String>,
) -> Option<String> {
    let selected = match intent {
        ReforgerSessionIntent::Configured => return None,
        ReforgerSessionIntent::New => return None,
        ReforgerSessionIntent::Latest => inventory
            .sessions
            .iter()
            .filter(|session| session.compatible)
            .max_by_key(|session| (session.saved_unix, session.playthrough, session.savepoint)),
        ReforgerSessionIntent::Selected { save_id } => inventory
            .sessions
            .iter()
            .find(|session| &session.save_id == save_id),
    };
    match selected {
        Some(session) if session.compatible => Some(session.save_id.clone()),
        Some(session) => {
            blockers.extend(session.blockers.iter().cloned());
            None
        }
        None => {
            blockers.push("The requested Reforger save is unavailable or incompatible.".into());
            None
        }
    }
}

pub(super) fn digest(value: &impl Serialize) -> Result<String, ServerStoreError> {
    let bytes = serde_json::to_vec(value).map_err(|_| ServerStoreError::Corrupt)?;
    Ok(blake3::hash(&bytes).to_hex().to_string())
}

fn write_document(
    path: &std::path::Path,
    document: &IntentDocument,
    control: &ContentStageControl,
) -> Result<(), ServerStoreError> {
    use std::io::Write;
    let bytes = serde_json::to_vec_pretty(document).map_err(|_| ServerStoreError::Corrupt)?;
    control.checkpoint()?;
    let parent = path.parent().ok_or(ServerStoreError::UnsafePath)?;
    crate::core::server::store::check_directory_chain(parent)?;
    crate::core::server::store::check_regular_file(path)?;
    std::fs::create_dir_all(parent).map_err(|_| ServerStoreError::Io)?;
    let mut temp = tempfile::NamedTempFile::new_in(parent).map_err(|_| ServerStoreError::Io)?;
    temp.write_all(&bytes)
        .and_then(|()| temp.as_file().sync_all())
        .map_err(|_| ServerStoreError::Io)?;
    control.checkpoint()?;
    temp.persist(path).map_err(|_| ServerStoreError::Io)?;
    #[cfg(unix)]
    std::fs::File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|_| ServerStoreError::Io)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uncovered_provider_preserves_configured_launch_but_blocks_automatic_selection() {
        let provider = ReforgerProviderCoverage {
            local_default: false,
            blockers: vec!["Custom storage is uncovered".into()],
        };
        assert!(
            provider
                .intent_blockers(&ReforgerSessionIntent::Configured)
                .is_empty()
        );
        for intent in [
            ReforgerSessionIntent::Latest,
            ReforgerSessionIntent::Selected {
                save_id: "01234567-89ab-cdef-0123-456789abcdef".into(),
            },
            ReforgerSessionIntent::New,
        ] {
            assert_eq!(provider.intent_blockers(&intent), provider.blockers);
        }
    }

    fn session(save_id: &str, saved_unix: i64, compatible: bool) -> ReforgerSessionSummary {
        ReforgerSessionSummary {
            save_id: save_id.into(),
            scenario: "scenario".into(),
            game_version: "1.9.0.37".into(),
            saved_unix,
            playthrough: 1,
            savepoint: 1,
            relative_path: "scenario/playthrough1/savepoint1/meta-info.json".into(),
            bytes: 10,
            blake3: "a".repeat(64),
            compatible,
            blockers: if compatible {
                Vec::new()
            } else {
                vec!["incompatible".into()]
            },
        }
    }

    #[test]
    fn latest_chooses_newest_compatible_and_never_falls_back_for_selected_uuid() {
        let inventory = ReforgerSessionInventory {
            instance_id: ServerInstanceId::new(),
            generation: 0,
            intent: ReforgerSessionIntent::Configured,
            sessions: vec![session("older", 1, true), session("newer", 2, false)],
            provider: ReforgerProviderCoverage {
                local_default: true,
                blockers: Vec::new(),
            },
            fingerprint: "b".repeat(64),
            writes_performed: false,
        };
        let mut blockers = Vec::new();
        assert_eq!(
            select_session(&inventory, &ReforgerSessionIntent::Latest, &mut blockers),
            Some("older".into())
        );
        assert!(blockers.is_empty());

        let mut blockers = Vec::new();
        assert_eq!(
            select_session(
                &inventory,
                &ReforgerSessionIntent::Selected {
                    save_id: "newer".into(),
                },
                &mut blockers,
            ),
            None
        );
        assert_eq!(blockers, vec!["incompatible"]);
    }

    #[test]
    fn latest_without_a_compatible_save_is_blocked() {
        let inventory = ReforgerSessionInventory {
            instance_id: ServerInstanceId::new(),
            generation: 0,
            intent: ReforgerSessionIntent::Configured,
            sessions: vec![session("old", 1, false)],
            provider: ReforgerProviderCoverage {
                local_default: true,
                blockers: Vec::new(),
            },
            fingerprint: "b".repeat(64),
            writes_performed: false,
        };
        let mut blockers = Vec::new();
        assert_eq!(
            select_session(&inventory, &ReforgerSessionIntent::Latest, &mut blockers),
            None
        );
        assert_eq!(blockers.len(), 1);
    }
}
