use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use foxy_server_backend_cli::{PlayerMod, verify_repository_controlled, write_player_repository};
use serde_json::{Value, json};

use super::super::content_staging::{
    StageEntry, StageEntryKind, publish_no_replace, read_bounded_controlled,
    verify_publication_tree,
};
use super::super::store::{check_directory_chain, check_regular_file};
use super::{ContentStageControl, Error, Payload, Reviewed};
use crate::core::server::ContentStagePhase;

const FRAGMENT: &str = "reforger_addons.json";

pub(super) fn publish(
    review: &Reviewed,
    control: &ContentStageControl,
    check_selection: impl FnOnce() -> Result<(), Error>,
) -> Result<(), Error> {
    let destination = &review.destination;
    if review.plan.destination_exists {
        return Err(Error::Invalid(
            "Publication requires a new output folder.".into(),
        ));
    }
    let parent = destination.parent().ok_or(Error::UnsafePath)?;
    check_directory_chain(parent)?;
    if fs4::available_space(parent).map_err(|_| Error::Io)? < review.plan.bytes {
        return Err(Error::Invalid(
            "The output volume has insufficient space for the selected content.".into(),
        ));
    }
    control.checkpoint()?;
    let mut builder = tempfile::Builder::new();
    builder.prefix(".foxy-player-publish-");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        builder.permissions(fs::Permissions::from_mode(0o700));
    }
    let stage = builder.tempdir_in(parent).map_err(|_| Error::Io)?;
    let result = (|| {
        control.start_phase(
            ContentStagePhase::Copying,
            review.plan.files,
            review.plan.bytes,
        );
        match &review.payload {
            Payload::Arma(mods) => write_player_repository(
                stage.path(),
                &review.plan.request.repository_name,
                mods,
                &|| control.checkpoint().is_err(),
                &|bytes| control.record_bytes(bytes),
            )
            .map_err(|_| publisher_failure(control))?,
            Payload::Reforger(bytes) => {
                fs::write(stage.path().join(FRAGMENT), bytes).map_err(|_| Error::Io)?;
                control.record_bytes(bytes.len() as u64);
                control.record_file();
            }
        }
        verify(review, stage.path(), control)?;
        check_selection()?;
        if !publish_no_replace(stage.path(), destination)? {
            return Err(Error::Invalid(
                "The destination appeared during publication. Existing data was preserved.".into(),
            ));
        }
        Ok(())
    })();
    match result {
        Ok(()) => {
            let _ = stage.keep();
            Ok(())
        }
        Err(error) => {
            stage.close().map_err(|_| Error::StageCleanupIncomplete)?;
            Err(error)
        }
    }
}

fn publisher_failure(control: &ContentStageControl) -> Error {
    control.checkpoint().err().unwrap_or_else(|| {
        Error::Invalid("Could not generate or verify the selected player repository.".into())
    })
}

pub(super) fn verify(
    review: &Reviewed,
    output: &Path,
    control: &ContentStageControl,
) -> Result<(), Error> {
    check_directory_chain(output)?;
    if !output.is_dir() {
        return Err(Error::Corrupt);
    }
    match &review.payload {
        Payload::Reforger(bytes) => verify_publication_tree(
            &[file_entry(FRAGMENT.into(), bytes)],
            bytes.len() as u64,
            output,
            control,
        ),
        Payload::Arma(mods) => verify_arma(review, output, mods, control),
    }
}

fn verify_arma(
    review: &Reviewed,
    output: &Path,
    mods: &[PlayerMod],
    control: &ContentStageControl,
) -> Result<(), Error> {
    let mut entries = BTreeMap::new();
    let repo = read_artifact(output, "repo.json", &mut entries, control)?;
    let repo: Value = serde_json::from_slice(&repo).map_err(|_| Error::Corrupt)?;
    if repo["repoName"] != review.plan.request.repository_name
        || repo["repoBasicAuthentication"] != json!({"username":"","password":""})
        || repo["servers"] != json!([])
        || repo["clientParameters"] != ""
        || repo["requiredMods"] != json!([])
        || repo["optionalMods"] != json!([])
        || repo["game"] != Value::Null
        || repo["appUpdateUrl"] != Value::Null
        || repo["gameBuild"] != Value::Null
        || repo["iconImagePath"] != ""
        || repo["repoImagePath"] != ""
        || !repo["foxyMode"].is_string()
    {
        return Err(Error::Corrupt);
    }
    let listing = read_artifact(output, "foxy_addons.json", &mut entries, control)?;
    let listing: Value = serde_json::from_slice(&listing).map_err(|_| Error::Corrupt)?;
    for (name, required) in [("requiredMods", true), ("optionalMods", false)] {
        let rows = listing[name].as_array().ok_or(Error::Corrupt)?;
        let selected = mods.iter().filter(|item| item.required == required);
        if rows.len() != selected.clone().count()
            || !rows.iter().zip(selected).all(|(row, item)| {
                row["modName"] == item.name
                    && row["enabled"] == required
                    && row.get("clientSide").is_none()
            })
        {
            return Err(Error::Corrupt);
        }
    }
    let line = read_artifact(output, "server_mod_line.txt", &mut entries, control)?;
    let mut expected_line = String::from("-mod=");
    for item in mods.iter().filter(|item| item.required) {
        expected_line.push_str(&item.name);
        expected_line.push(';');
    }
    expected_line.push('\n');
    if line != expected_line.as_bytes() {
        return Err(Error::Corrupt);
    }
    for item in mods {
        read_artifact(
            output,
            &format!("{}/foxy_addon.json", item.name),
            &mut entries,
            control,
        )?;
        insert_directory(&mut entries, &item.name);
        for file in &item.files {
            let relative = file
                .relative_path
                .to_str()
                .ok_or(Error::Corrupt)?
                .replace('\\', "/");
            let path = format!("{}/{relative}", item.name);
            if let Some((dir, _)) = path.rsplit_once('/') {
                insert_directory(&mut entries, dir);
            }
            entries.insert(
                path.clone(),
                StageEntry {
                    path,
                    kind: StageEntryKind::File,
                    bytes: file.bytes,
                    blake3: Some(file.blake3.clone()),
                },
            );
        }
    }
    let entries = entries.into_values().collect::<Vec<_>>();
    let bytes = entries.iter().map(|entry| entry.bytes).sum();
    verify_publication_tree(&entries, bytes, output, control)?;
    let verified = verify_repository_controlled(output, &|| control.checkpoint().is_err())
        .map_err(|_| publisher_failure(control))?;
    if verified.repositories != 1 || verified.mods != mods.len() {
        return Err(Error::Corrupt);
    }
    control.checkpoint()
}

fn read_artifact(
    output: &Path,
    relative: &str,
    entries: &mut BTreeMap<String, StageEntry>,
    control: &ContentStageControl,
) -> Result<Vec<u8>, Error> {
    let path = output.join(relative);
    check_directory_chain(path.parent().ok_or(Error::UnsafePath)?)?;
    check_regular_file(&path)?;
    let bytes = read_bounded_controlled(&path, 32 * 1024 * 1024, control)?;
    entries.insert(relative.into(), file_entry(relative.into(), &bytes));
    Ok(bytes)
}

fn file_entry(path: String, bytes: &[u8]) -> StageEntry {
    StageEntry {
        path,
        kind: StageEntryKind::File,
        bytes: bytes.len() as u64,
        blake3: Some(blake3::hash(bytes).to_hex().to_string()),
    }
}

fn insert_directory(entries: &mut BTreeMap<String, StageEntry>, dir: &str) {
    for (index, _) in dir
        .match_indices('/')
        .chain(std::iter::once((dir.len(), "")))
    {
        let prefix = &dir[..index];
        entries.entry(prefix.into()).or_insert_with(|| StageEntry {
            path: prefix.into(),
            kind: StageEntryKind::Directory,
            bytes: 0,
            blake3: None,
        });
    }
}

#[cfg(all(test, any(windows, target_os = "linux")))]
mod tests {
    use super::*;
    use crate::core::server::{
        PlayerPublicationPlan, PlayerPublicationRequest, ServerDefinitionKind,
    };

    #[test]
    fn cancel_and_raced_destination_preserve_existing_files() {
        let parent = tempfile::tempdir().unwrap();
        let destination = parent.path().join("public");
        let review = Reviewed {
            plan: PlayerPublicationPlan {
                request: PlayerPublicationRequest {
                    instance_id: Default::default(),
                    output: destination.clone(),
                    repository_name: "Player content".into(),
                    recommendations: Vec::new(),
                    include_public_keys: false,
                },
                game_space_id: "alpha".into(),
                game: ServerDefinitionKind::Reforger,
                record_id: "fixture".into(),
                fingerprint: String::new(),
                arma_addons: Vec::new(),
                reforger: None,
                files: 1,
                bytes: 2,
                destination_exists: false,
                writes_performed: false,
            },
            payload: Payload::Reforger(b"{}".to_vec()),
            selection: String::new(),
            destination: destination.clone(),
        };
        let cancelled = ContentStageControl::default();
        cancelled.cancel_on_phase_for_test(ContentStagePhase::Copying);
        assert_eq!(
            publish(&review, &cancelled, || panic!("cancelled before promotion")),
            Err(Error::Cancelled)
        );
        assert_eq!(fs::read_dir(parent.path()).unwrap().count(), 0);

        let outcome = publish(&review, &ContentStageControl::default(), || {
            fs::create_dir(&destination).unwrap();
            fs::write(destination.join("operator.txt"), b"preserve").unwrap();
            Ok(())
        });
        assert!(outcome.is_err());
        assert_eq!(fs::read_dir(parent.path()).unwrap().count(), 1);
        assert_eq!(fs::read_dir(&destination).unwrap().count(), 1);
        assert_eq!(
            fs::read(destination.join("operator.txt")).unwrap(),
            b"preserve"
        );
    }
}
