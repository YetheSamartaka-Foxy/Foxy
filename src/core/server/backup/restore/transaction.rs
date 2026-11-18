use super::*;

impl ServerStore {
    pub(super) fn read_restore_journal(
        &self,
        id: &ServerInstanceId,
    ) -> Result<Option<(RestoreJournal, String)>, ServerStoreError> {
        let root = self.restore_root(id);
        check_directory_chain(&root)?;
        let path = root.join("journal.json");
        check_regular_file(&path)?;
        if !path.try_exists().map_err(|_| ServerStoreError::Io)? {
            return Ok(None);
        }
        let bytes = read_bounded_controlled(
            &path,
            MAX_BACKUP_MANIFEST_BYTES,
            &ContentStageControl::default(),
        )?;
        let journal: RestoreJournal =
            serde_json::from_slice(&bytes).map_err(|_| ServerStoreError::Corrupt)?;
        if journal.schema_version != 1
            || journal.game_space_id != self.context().space_id()
            || journal.game_id != self.context().game_id()
            || &journal.instance_id != id
            || !valid_restore_id(&journal.restore_id)
            || !super::super::model::valid_backup_id(&journal.backup_id)
            || !valid_lower_hex(&journal.plan_fingerprint, 64)
            || journal.entries.is_empty()
            || journal.entries.len() > super::super::model::MAX_BACKUP_FILES
        {
            return Err(ServerStoreError::Corrupt);
        }
        let mut names = BTreeSet::new();
        let mut total = 0u64;
        let mut before_total = 0u64;
        for entry in &journal.entries {
            if save_target(&format!("saves/{}", entry.path))? != Some(entry.path.as_str())
                || !valid_lower_hex(&entry.blake3, 64)
                || entry.bytes > super::super::model::MAX_BACKUP_SINGLE_FILE_BYTES
                || !names.insert(entry.path.to_ascii_lowercase())
            {
                return Err(ServerStoreError::Corrupt);
            }
            total = total
                .checked_add(entry.bytes)
                .ok_or(ServerStoreError::Corrupt)?;
            if let Some((bytes, hash)) = &entry.prior {
                if !valid_lower_hex(hash, 64)
                    || *bytes > super::super::model::MAX_BACKUP_SINGLE_FILE_BYTES
                {
                    return Err(ServerStoreError::Corrupt);
                }
                before_total = before_total
                    .checked_add(*bytes)
                    .ok_or(ServerStoreError::Corrupt)?;
            }
        }
        if total > MAX_BACKUP_TOTAL_BYTES || before_total > MAX_ROLLBACK_BYTES {
            return Err(ServerStoreError::Corrupt);
        }
        for name in &names {
            let mut prefix = name.as_str();
            while let Some((parent, _)) = prefix.rsplit_once('/') {
                if names.contains(parent) {
                    return Err(ServerStoreError::Corrupt);
                }
                prefix = parent;
            }
        }
        Ok(Some((journal, blake3::hash(&bytes).to_hex().to_string())))
    }

    pub fn restore_server_backup(
        &self,
        id: &ServerInstanceId,
        backup_id: &str,
        expected: &str,
        witness: Option<&InstanceRuntimeStatus>,
        control: &ContentStageControl,
    ) -> Result<ServerRestoreOutcome, ServerStoreError> {
        let result = (|| {
            let _pin = crate::core::server::owner::pin_write(&self.context().database_path())
                .map_err(|_| ServerStoreError::Ownership)?;
            let (plan, manifest, entries) =
                self.restore_plan_internal(id, backup_id, witness, control)?;
            if !valid_lower_hex(expected, 64) || plan.fingerprint != expected {
                return Err(ServerStoreError::StaleRevision);
            }
            if !plan.blockers.is_empty() {
                return Err(ServerStoreError::Invalid(plan.blockers.join(" ")));
            }
            let root = self.restore_root(id);
            check_directory_chain(&root)?;
            fs::create_dir_all(&root).map_err(|_| ServerStoreError::Io)?;
            let stage = tempfile::Builder::new()
                .prefix(".restore-build-")
                .tempdir_in(&root)
                .map_err(|_| ServerStoreError::Io)?;
            let source = self.backups_root(id).join(backup_id).join("archive.zip");
            copy_verified(
                &source,
                &stage.path().join("archive.zip"),
                &(
                    fs::metadata(&source)
                        .map_err(|_| ServerStoreError::Io)?
                        .len(),
                    plan.archive_blake3.clone(),
                ),
                control,
            )?;
            copy_verified(
                &self.backups_root(id).join(backup_id).join("manifest.json"),
                &stage.path().join("manifest.json"),
                &hash_and_measure_file(
                    &self.backups_root(id).join(backup_id).join("manifest.json"),
                    control,
                )?,
                control,
            )?;
            let (_, staged_manifest) =
                verify_backup_directory(self, id, backup_id, stage.path(), control)?;
            if staged_manifest != manifest {
                return Err(ServerStoreError::StaleRevision);
            }
            extract_all(stage.path(), &manifest, control)?;
            let state = self.record_root(id).join("runtime-state");
            for entry in &entries {
                if measure_optional(&state.join(&entry.path), control)? != entry.prior {
                    return Err(ServerStoreError::StaleRevision);
                }
                if let Some(prior) = &entry.prior {
                    copy_verified(
                        &state.join(&entry.path),
                        &stage.path().join("before").join(&entry.path),
                        prior,
                        control,
                    )?;
                }
            }
            let journal = RestoreJournal {
                schema_version: 1,
                game_space_id: self.context().space_id().into(),
                game_id: self.context().game_id().into(),
                instance_id: id.clone(),
                restore_id: format!("restore_{}", digest(&(expected, rand::random::<u128>()))?),
                backup_id: backup_id.into(),
                plan_fingerprint: expected.into(),
                entries,
            };
            control.checkpoint()?;
            for entry in &journal.entries {
                if measure_optional(&state.join(&entry.path), control)? != entry.prior {
                    return Err(ServerStoreError::StaleRevision);
                }
            }
            let tx = root.join(&journal.restore_id);
            if !crate::core::server::content_staging::publish_no_replace(stage.path(), &tx)? {
                return Err(ServerStoreError::StaleRevision);
            }
            let _ = stage.keep();
            sync_dir(&root)?;
            write_json(&root.join("journal.json"), &journal)?;
            // Cancellation after publication keeps the journal and rollback bytes until reviewed recovery.
            self.promote_restore(&journal, &tx, control)?;
            let outcome = restore_outcome(&journal, false);
            self.finish_restore(&journal, &outcome)?;
            Ok(outcome)
        })();
        control.finish(&result);
        result
    }

    pub(super) fn promote_restore(
        &self,
        journal: &RestoreJournal,
        tx: &Path,
        control: &ContentStageControl,
    ) -> Result<(), ServerStoreError> {
        let state = self.record_root(&journal.instance_id).join("runtime-state");
        control.start_phase(
            crate::core::server::ContentStagePhase::Copying,
            journal.entries.len() as u64,
            journal.entries.iter().map(|entry| entry.bytes).sum(),
        );
        for entry in &journal.entries {
            control.checkpoint()?;
            if measure_optional(&state.join(&entry.path), control)? != entry.prior {
                return Err(ServerStoreError::StaleRevision);
            }
            replace_verified(
                &tx.join("validated/saves").join(&entry.path),
                &state.join(&entry.path),
                &(entry.bytes, entry.blake3.clone()),
                control,
            )?;
            control.record_file();
        }
        control.start_phase(
            crate::core::server::ContentStagePhase::Checking,
            journal.entries.len() as u64,
            journal.entries.iter().map(|entry| entry.bytes).sum(),
        );
        for entry in &journal.entries {
            if measure_optional(&state.join(&entry.path), control)?
                != Some((entry.bytes, entry.blake3.clone()))
            {
                return Err(ServerStoreError::StaleRevision);
            }
        }
        Ok(())
    }

    pub fn rollback_server_restore(
        &self,
        id: &ServerInstanceId,
        expected: &str,
        witness: Option<&InstanceRuntimeStatus>,
        control: &ContentStageControl,
    ) -> Result<ServerRestoreOutcome, ServerStoreError> {
        let result = (|| {
            let _pin = crate::core::server::owner::pin_write(&self.context().database_path())
                .map_err(|_| ServerStoreError::Ownership)?;
            let (journal, fingerprint) = self
                .read_restore_journal(id)?
                .ok_or(ServerStoreError::NotFound)?;
            if !valid_lower_hex(expected, 64) || fingerprint != expected {
                return Err(ServerStoreError::StaleRevision);
            }
            let catalog = self.load()?;
            let instance = self.show(id)?;
            let installation = instance.installation_id.as_ref().and_then(|key| {
                catalog
                    .installations
                    .iter()
                    .find(|installation| &installation.id == key)
            });
            let (consistency, blockers) = super::super::archive::inspect_consistency_witness(
                self,
                id,
                &instance,
                &catalog.instances,
                installation,
                BackupConsistencyMode::StoppedVerified,
                witness,
            )?;
            if !consistency.stopped_verified || !blockers.is_empty() {
                return Err(ServerStoreError::Invalid(blockers.join(" ")));
            }
            let state = self.record_root(id).join("runtime-state");
            let tx = self.restore_root(id).join(&journal.restore_id);
            check_directory_chain(&tx)?;
            // Check every preimage and destination before the first rollback write; unknown changes stay protected.
            for entry in &journal.entries {
                control.checkpoint()?;
                if let Some(prior) = &entry.prior
                    && measure_optional(&tx.join("before").join(&entry.path), control)?.as_ref()
                        != Some(prior)
                {
                    return Err(ServerStoreError::Corrupt);
                }
                let actual = measure_optional(&state.join(&entry.path), control)?;
                if actual != entry.prior && actual != Some((entry.bytes, entry.blake3.clone())) {
                    return Err(ServerStoreError::Invalid("A restore destination changed outside this transaction. Preserve it and resolve the conflict before rollback.".into()));
                }
            }
            for entry in journal.entries.iter().rev() {
                control.checkpoint()?;
                let target = state.join(&entry.path);
                let actual = measure_optional(&target, control)?;
                if actual == entry.prior {
                    continue;
                }
                if actual != Some((entry.bytes, entry.blake3.clone())) {
                    return Err(ServerStoreError::StaleRevision);
                }
                if let Some(prior) = &entry.prior {
                    replace_verified(
                        &tx.join("before").join(&entry.path),
                        &target,
                        prior,
                        control,
                    )?;
                } else {
                    fs::remove_file(&target).map_err(|_| ServerStoreError::Io)?;
                    sync_dir(target.parent().ok_or(ServerStoreError::UnsafePath)?)?;
                }
            }
            for entry in &journal.entries {
                if measure_optional(&state.join(&entry.path), control)? != entry.prior {
                    return Err(ServerStoreError::StaleRevision);
                }
            }
            let outcome = restore_outcome(&journal, true);
            self.finish_restore(&journal, &outcome)?;
            Ok(outcome)
        })();
        control.finish(&result);
        result
    }

    fn finish_restore(
        &self,
        journal: &RestoreJournal,
        outcome: &ServerRestoreOutcome,
    ) -> Result<(), ServerStoreError> {
        let root = self.restore_root(&journal.instance_id);
        write_json(
            &root
                .join("receipts")
                .join(format!("{}.json", journal.restore_id)),
            outcome,
        )?;
        let expected = serde_json::to_vec_pretty(journal).map_err(|_| ServerStoreError::Corrupt)?;
        let path = root.join("journal.json");
        if read_bounded_controlled(
            &path,
            MAX_BACKUP_MANIFEST_BYTES,
            &ContentStageControl::default(),
        )? != expected
        {
            return Err(ServerStoreError::StaleRevision);
        }
        fs::remove_file(path).map_err(|_| ServerStoreError::Io)?;
        sync_dir(&root)?;
        cleanup_transaction(&root.join(&journal.restore_id), journal);
        Ok(())
    }

    pub fn load_server_restore_outcome(
        &self,
        id: &ServerInstanceId,
        restore_id: &str,
        control: &ContentStageControl,
    ) -> Result<ServerRestoreOutcome, ServerStoreError> {
        if !valid_restore_id(restore_id) {
            return Err(ServerStoreError::Corrupt);
        }
        self.validate_record_context()?;
        self.show(id)?;
        self.require_no_server_restore(id)?;
        let root = self.restore_root(id).join("receipts");
        check_directory_chain(&root)?;
        let path = root.join(format!("{restore_id}.json"));
        check_regular_file(&path)?;
        let bytes = read_bounded_controlled(&path, 16 * 1024, control)?;
        let outcome: ServerRestoreOutcome =
            serde_json::from_slice(&bytes).map_err(|_| ServerStoreError::Corrupt)?;
        if outcome.schema_version != 1
            || outcome.game_space_id != self.context().space_id()
            || outcome.game_id != self.context().game_id()
            || &outcome.instance_id != id
            || outcome.restore_id != restore_id
            || !super::super::model::valid_backup_id(&outcome.backup_id)
            || !valid_lower_hex(&outcome.plan_fingerprint, 64)
        {
            return Err(ServerStoreError::Corrupt);
        }
        Ok(outcome)
    }
}

fn restore_outcome(journal: &RestoreJournal, rolled_back: bool) -> ServerRestoreOutcome {
    ServerRestoreOutcome {
        schema_version: 1,
        game_space_id: journal.game_space_id.clone(),
        game_id: journal.game_id.clone(),
        restore_id: journal.restore_id.clone(),
        instance_id: journal.instance_id.clone(),
        backup_id: journal.backup_id.clone(),
        plan_fingerprint: journal.plan_fingerprint.clone(),
        restored_files: journal.entries.len(),
        restored_bytes: journal.entries.iter().map(|entry| entry.bytes).sum(),
        rolled_back,
        writes_performed: true,
    }
}

fn replace_verified(
    source: &Path,
    target: &Path,
    expected: &(u64, String),
    control: &ContentStageControl,
) -> Result<(), ServerStoreError> {
    let parent = target.parent().ok_or(ServerStoreError::UnsafePath)?;
    check_directory_chain(parent)?;
    check_regular_file(target)?;
    check_regular_file(source)?;
    fs::create_dir_all(parent).map_err(|_| ServerStoreError::Io)?;
    let mut temp = tempfile::Builder::new()
        .prefix(".foxy-restore-")
        .tempfile_in(parent)
        .map_err(|_| ServerStoreError::Io)?;
    stream_verified(
        &mut File::open(source).map_err(|_| ServerStoreError::Io)?,
        &mut temp,
        expected,
        control,
    )?;
    temp.as_file()
        .sync_all()
        .map_err(|_| ServerStoreError::Io)?;
    control.checkpoint()?;
    check_directory_chain(parent)?;
    check_regular_file(target)?;
    temp.persist(target).map_err(|_| ServerStoreError::Io)?;
    sync_dir(parent)
}

fn extract_all(
    root: &Path,
    manifest: &ServerBackupManifest,
    control: &ContentStageControl,
) -> Result<(), ServerStoreError> {
    let mut zip =
        ZipArchive::new(File::open(root.join("archive.zip")).map_err(|_| ServerStoreError::Io)?)
            .map_err(|_| ServerStoreError::Corrupt)?;
    let mut names = BTreeSet::new();
    for entry in &manifest.entries {
        control.checkpoint()?;
        if !portable_path(&entry.archive_path)
            || !names.insert(entry.archive_path.to_ascii_lowercase())
        {
            return Err(ServerStoreError::UnsafePath);
        }
        let target = root.join("validated").join(&entry.archive_path);
        let parent = target.parent().ok_or(ServerStoreError::UnsafePath)?;
        check_directory_chain(parent)?;
        fs::create_dir_all(parent).map_err(|_| ServerStoreError::Io)?;
        let mut output = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&target)
            .map_err(|_| ServerStoreError::Io)?;
        let mut input = zip
            .by_name(&entry.archive_path)
            .map_err(|_| ServerStoreError::Corrupt)?;
        stream_verified(
            &mut input,
            &mut output,
            &(entry.size_bytes, entry.blake3.clone()),
            control,
        )?;
        output.sync_all().map_err(|_| ServerStoreError::Io)?;
        sync_dir(parent)?;
    }
    Ok(())
}

fn cleanup_transaction(root: &Path, journal: &RestoreJournal) {
    // Never traverse or remove unexpected entries after a completed transaction.
    if check_directory_chain(root).is_err() {
        return;
    }
    let control = ContentStageControl::default();
    let manifest_path = root.join("manifest.json");
    let manifest = read_bounded_controlled(&manifest_path, MAX_BACKUP_MANIFEST_BYTES, &control)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<ServerBackupManifest>(&bytes).ok())
        .filter(|manifest| {
            manifest.validate().is_ok()
                && manifest.backup_id == journal.backup_id
                && manifest.instance_id == journal.instance_id
        });
    if let Some(manifest) = manifest {
        for entry in &manifest.entries {
            let path = root.join("validated").join(&entry.archive_path);
            if portable_path(&entry.archive_path)
                && hash_and_measure_file(&path, &control).ok()
                    == Some((entry.size_bytes, entry.blake3.clone()))
            {
                let _ = fs::remove_file(path);
            }
        }
        for entry in &journal.entries {
            let path = root.join("before").join(&entry.path);
            if measure_optional(&path, &control).ok().flatten() == entry.prior
                && entry.prior.is_some()
            {
                let _ = fs::remove_file(path);
            }
        }
        for name in ["archive.zip", "manifest.json"] {
            let path = root.join(name);
            if check_regular_file(&path).is_ok() {
                let _ = fs::remove_file(path);
            }
        }
    }
    for name in ["validated", "before"] {
        remove_empty_directories(&root.join(name), 0);
    }
    let _ = fs::remove_dir(root);
}

fn remove_empty_directories(root: &Path, depth: usize) {
    if depth > 64 || check_directory_chain(root).is_err() {
        return;
    }
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if check_directory_chain(&path).is_ok() {
            remove_empty_directories(&path, depth + 1);
        }
    }
    let _ = fs::remove_dir(root);
}
