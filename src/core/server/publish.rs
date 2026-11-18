mod arma;
mod output;

use std::path::{Path, PathBuf};

use foxy_server_backend_cli::{PlayerMod, ReforgerModReference, reforger_addon_fragment};
use serde::Serialize;

use super::content_staging::physical_directory;
use super::{
    ContentStageControl, ReforgerAddonLock, ReforgerAddonProvider, ServerAddonRole,
    ServerDefinition, ServerDefinitionKind, ServerInstanceId, ServerStore,
    ServerStoreError as Error,
};

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct PlayerPublicationRequest {
    pub instance_id: ServerInstanceId,
    pub output: PathBuf,
    pub repository_name: String,
    pub recommendations: Vec<String>,
    pub include_public_keys: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct PlayerPublicationPlan {
    pub request: PlayerPublicationRequest,
    pub game_space_id: String,
    pub game: ServerDefinitionKind,
    pub record_id: String,
    pub fingerprint: String,
    pub arma_addons: Vec<PlayerAddonReview>,
    pub reforger: Option<ReforgerPlayerReview>,
    pub files: u64,
    pub bytes: u64,
    pub destination_exists: bool,
    pub writes_performed: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct PlayerAddonReview {
    pub profile: String,
    pub name: String,
    pub role: ServerAddonRole,
    pub selected: bool,
    pub files: u64,
    pub bytes: u64,
    pub excluded_files: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct ReforgerPlayerReview {
    pub scenario_id: String,
    pub scenario_provider: Option<String>,
    pub addons: Vec<ReforgerPlayerAddon>,
    pub omissions: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ReforgerPlayerAddon {
    pub id: String,
    pub version: String,
    pub required: bool,
    pub provider: ReforgerAddonProvider,
}

#[derive(Clone, Debug, Serialize)]
pub struct PlayerPublicationOutcome {
    pub output: PathBuf,
    pub record_id: String,
    pub verified: bool,
    pub writes_performed: bool,
}

enum Payload {
    Arma(Vec<PlayerMod>),
    Reforger(Vec<u8>),
}

struct Reviewed {
    plan: PlayerPublicationPlan,
    payload: Payload,
    selection: String,
    destination: PathBuf,
}

impl ServerStore {
    pub fn plan_player_publication(
        &self,
        request: &PlayerPublicationRequest,
        control: &ContentStageControl,
    ) -> Result<PlayerPublicationPlan, Error> {
        let result = self
            .review_publication(request, control)
            .map(|review| review.plan);
        control.finish(&result);
        result
    }

    pub fn apply_player_publication(
        &self,
        plan: &PlayerPublicationPlan,
        control: &ContentStageControl,
    ) -> Result<PlayerPublicationOutcome, Error> {
        self.apply_player_publication_request(&plan.request, &plan.fingerprint, control)
    }

    pub fn verify_player_publication(
        &self,
        plan: &PlayerPublicationPlan,
        control: &ContentStageControl,
    ) -> Result<PlayerPublicationOutcome, Error> {
        self.verify_player_publication_request(&plan.request, Some(&plan.fingerprint), control)
    }

    pub fn apply_player_publication_request(
        &self,
        request: &PlayerPublicationRequest,
        expected: &str,
        control: &ContentStageControl,
    ) -> Result<PlayerPublicationOutcome, Error> {
        self.execute_publication(request, Some(expected), control, true)
    }

    pub fn verify_player_publication_request(
        &self,
        request: &PlayerPublicationRequest,
        expected: Option<&str>,
        control: &ContentStageControl,
    ) -> Result<PlayerPublicationOutcome, Error> {
        self.execute_publication(request, expected, control, false)
    }

    fn execute_publication(
        &self,
        request: &PlayerPublicationRequest,
        expected: Option<&str>,
        control: &ContentStageControl,
        publish: bool,
    ) -> Result<PlayerPublicationOutcome, Error> {
        let result = (|| {
            let reviewed = self.review_publication(request, control)?;
            if expected.is_some_and(|fingerprint| reviewed.plan.fingerprint != fingerprint) {
                return Err(Error::StaleRevision);
            }
            if publish {
                output::publish(&reviewed, control, || {
                    self.check_publication_selection(&reviewed, control)
                })?;
            } else {
                output::verify(&reviewed, &reviewed.destination, control)?;
                self.check_publication_selection(&reviewed, control)?;
            }
            Ok(PlayerPublicationOutcome {
                output: reviewed.plan.request.output,
                record_id: reviewed.plan.record_id,
                verified: true,
                writes_performed: publish,
            })
        })();
        control.finish(&result);
        result
    }

    fn check_publication_selection(
        &self,
        reviewed: &Reviewed,
        control: &ContentStageControl,
    ) -> Result<(), Error> {
        self.validate_record_context()?;
        let (_, selection) = self.read_record_state(&reviewed.plan.request.instance_id, control)?;
        if selection.as_deref() != Some(&reviewed.selection)
            || physical_directory(&reviewed.plan.request.output)? != reviewed.destination
        {
            return Err(Error::StaleRevision);
        }
        control.checkpoint()
    }

    fn review_publication(
        &self,
        request: &PlayerPublicationRequest,
        control: &ContentStageControl,
    ) -> Result<Reviewed, Error> {
        if !self.capabilities().player_publication {
            return Err(Error::UnsupportedGame);
        }
        self.validate_record_context()?;
        validate_request(request)?;
        let id = &request.instance_id;
        self.show(id)?;
        let (state, selection) = self.read_record_state(id, control)?;
        let record_id = state.selected.ok_or_else(|| {
            Error::Invalid(
                "Select a committed deployment record before reviewing publication.".into(),
            )
        })?;
        let selection = selection.ok_or(Error::Corrupt)?;
        let record = self.read_record(id, &record_id, control)?;
        let destination = physical_directory(&request.output)?;
        let data_root = physical_directory(self.context().data_root())?;
        if super::installation::paths_overlap(&destination, &data_root) {
            return Err(Error::UnsafePath);
        }
        self.check_deployment_destination(&destination, Some(&record.content_bindings))?;
        let parent = destination.parent().ok_or(Error::UnsafePath)?;
        if !parent.is_dir() {
            return Err(Error::Invalid(
                "The publication folder must have an existing parent.".into(),
            ));
        }
        let configuration = self
            .record_root(id)
            .parent()
            .ok_or(Error::Corrupt)?
            .join("configuration/revisions")
            .join(&record.configuration_revision);
        let stage = self.stage_root(id).join(&record.content_stage_id);
        let (_, manifest) =
            self.inspect_record_archives_at(id, &record, &configuration, &stage, control)?;
        let (instance, _, hash) = self.read_verified_configuration_at(
            id,
            &record.configuration_revision,
            &configuration,
            control,
        )?;
        if hash != record.configuration_manifest_fingerprint {
            return Err(Error::StaleRevision);
        }
        let (payload, arma_addons, reforger) = match instance.definition {
            ServerDefinition::Arma3(_) => {
                let (mods, review) = arma::project(request, &stage, &manifest, control)?;
                (Payload::Arma(mods), review, None)
            }
            ServerDefinition::Reforger(definition) => {
                if request.include_public_keys || !request.recommendations.is_empty() {
                    return Err(Error::Invalid(
                        "Recommendation and public-key selection applies to Arma 3 only.".into(),
                    ));
                }
                let review = reforger_review(
                    definition.scenario_id.ok_or(Error::Corrupt)?,
                    manifest.reforger_addons.as_ref(),
                );
                let bytes = reforger_addon_fragment(
                    &review
                        .addons
                        .iter()
                        .map(|addon| ReforgerModReference {
                            id: addon.id.clone(),
                            name: None,
                            version: Some(addon.version.clone()),
                            required: Some(addon.required),
                        })
                        .collect::<Vec<_>>(),
                )
                .map_err(|_| Error::Corrupt)?;
                (Payload::Reforger(bytes), Vec::new(), Some(review))
            }
        };
        let (files, bytes) = match &payload {
            Payload::Arma(mods) => (
                mods.iter().map(|item| item.files.len() as u64).sum(),
                mods.iter()
                    .flat_map(|item| &item.files)
                    .map(|file| file.bytes)
                    .sum(),
            ),
            Payload::Reforger(bytes) => (1, bytes.len() as u64),
        };
        let fingerprint = blake3::hash(
            &serde_json::to_vec(&(
                1,
                &data_root,
                self.context().space_id(),
                self.context().game_id(),
                &destination,
                &selection,
                &record_id,
                &request.repository_name,
                &request.recommendations,
                request.include_public_keys,
            ))
            .map_err(|_| Error::Corrupt)?,
        )
        .to_hex()
        .to_string();
        let reviewed = Reviewed {
            plan: PlayerPublicationPlan {
                request: request.clone(),
                game_space_id: self.context().space_id().into(),
                game: self
                    .capabilities()
                    .definition_kind
                    .ok_or(Error::UnsupportedGame)?,
                record_id,
                fingerprint,
                arma_addons,
                reforger,
                files,
                bytes,
                destination_exists: destination.try_exists().map_err(|_| Error::Io)?,
                writes_performed: false,
            },
            payload,
            selection,
            destination,
        };
        self.check_publication_selection(&reviewed, control)?;
        Ok(reviewed)
    }
}

fn validate_request(request: &PlayerPublicationRequest) -> Result<(), Error> {
    let output = &request.output;
    let name = output
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or(Error::UnsafePath)?;
    let parsed = super::content_staging::parse_relative(name)?;
    if !output.is_absolute() || parsed != Path::new(name) || parsed.components().count() != 1 {
        return Err(Error::UnsafePath);
    }
    let name = &request.repository_name;
    if name.trim().is_empty() || name.len() > 128 || name.chars().any(char::is_control) {
        return Err(Error::Invalid(
            "Enter a repository name of at most 128 bytes.".into(),
        ));
    }
    if request.recommendations.len() > 256 {
        return Err(Error::Invalid("Too many player recommendations.".into()));
    }
    Ok(())
}

fn reforger_review(scenario_id: String, lock: Option<&ReforgerAddonLock>) -> ReforgerPlayerReview {
    ReforgerPlayerReview {
        scenario_id,
        scenario_provider: lock.and_then(|lock| lock.scenario_provider.clone()),
        addons: lock
            .into_iter()
            .flat_map(|lock| &lock.realized)
            .map(|item| ReforgerPlayerAddon {
                id: item.addon_id.to_ascii_uppercase(),
                version: item.version.clone(),
                required: true,
                provider: item.provider,
            })
            .collect(),
        omissions: lock
            .into_iter()
            .flat_map(|lock| &lock.omissions)
            .map(|item| item.addon_id.to_ascii_uppercase())
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn reforger_projects_the_realized_provider_and_omissions_only() {
        let lock: ReforgerAddonLock = serde_json::from_value(json!({
            "channel": "stable",
            "requested": [
                {"addon_id":"1111111111111111"},
                {"addon_id":"3333333333333333","required":false}
            ],
            "mods_required_by_default": true,
            "scenario_id": "{AAAAAAAAAAAAAAAA}Missions/Example.conf",
            "scenario_provider": "1111111111111111",
            "realized": [
                {"addon_id":"1111111111111111","version":"1.2.3","channel":"stable","provider":"bohemia_workshop",
                 "dependencies":[{"addon_id":"2222222222222222","version":"2.1.0"}]},
                {"addon_id":"2222222222222222","version":"2.1.0","channel":"stable","provider":"local"}
            ],
            "omissions": [{"addon_id":"3333333333333333","reason":"optional unavailable"}],
            "degraded": true,
            "content_sources": {"2222222222222222":"R:/Mods/private-source"}
        })).unwrap();
        let report = reforger_review(lock.scenario_id.clone(), Some(&lock));
        assert_eq!(
            report.scenario_provider.as_deref(),
            Some("1111111111111111")
        );
        assert_eq!(
            report.scenario_id,
            "{AAAAAAAAAAAAAAAA}Missions/Example.conf"
        );
        assert_eq!(report.omissions, ["3333333333333333"]);
        assert_eq!(report.addons.len(), 2);
        assert_eq!(report.addons[0].version, "1.2.3");
        assert_eq!(
            report.addons[0].provider,
            ReforgerAddonProvider::BohemiaWorkshop
        );
        assert_eq!(report.addons[1].id, "2222222222222222");
        assert_eq!(report.addons[1].version, "2.1.0");
        assert!(report.addons.iter().all(|item| item.required));
        assert!(
            !serde_json::to_string(&report)
                .unwrap()
                .contains("private-source")
        );
        let mods = report
            .addons
            .into_iter()
            .map(|addon| ReforgerModReference {
                id: addon.id,
                name: None,
                version: Some(addon.version),
                required: Some(addon.required),
            })
            .collect::<Vec<_>>();
        let fragment: serde_json::Value =
            serde_json::from_slice(&reforger_addon_fragment(&mods).unwrap()).unwrap();
        let example: serde_json::Value = serde_json::from_str(include_str!(
            "../../../examples/json/server_backend/reforger_player_addons.json"
        ))
        .unwrap();
        assert_eq!(fragment, example);
    }
}
