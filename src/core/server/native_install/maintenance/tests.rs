use super::*;
use crate::core::server::installation::tests::create_native_install;

struct StoreClaim {
    path: PathBuf,
}

fn claim_store(store: &ServerStore) -> StoreClaim {
    let path = store.validate_claim_path().unwrap();
    assert_eq!(
        crate::core::tasks::db_process_lock::acquire(&path),
        crate::core::tasks::db_process_lock::LockOutcome::Acquired
    );
    StoreClaim { path }
}

impl Drop for StoreClaim {
    fn drop(&mut self) {
        crate::core::tasks::db_process_lock::release_for(&self.path).unwrap();
    }
}

fn isolated_claimed_test(test_name: &str, run: impl FnOnce()) {
    const PROBE: &str = "FOXY_NATIVE_MAINTENANCE_TEST_PROBE";
    if std::env::var_os(PROBE).is_none() {
        let test_name = test_name.split_once("::").unwrap().1;
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", test_name])
            .env(PROBE, "1")
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success() && stdout.contains("running 1 test"),
            "{stdout}\n{stderr}"
        );
        return;
    }
    run();
}

fn fixture_store(root: &Path) -> ServerStore {
    fixture_store_for(root, "arma3")
}

fn fixture_store_for(root: &Path, game: &str) -> ServerStore {
    fs::write(
        root.join("games.json"),
        format!(r#"{{"active_game_space_id":"alpha","last_opened_game_space_id":"alpha","game_spaces":[{{"id":"alpha","game_id":"{game}","display_name":"Alpha","created_at":0}}]}}"#),
    )
    .unwrap();
    ServerStore::new(ServerContext::capture(root, Some("alpha")).unwrap()).unwrap()
}

fn managed_store_fixture() -> (
    tempfile::TempDir,
    tempfile::TempDir,
    ServerStore,
    ServerInstanceId,
    StoreClaim,
) {
    managed_store_fixture_for(ServerDefinitionKind::Arma3, ContentChannel::Stable)
}

fn managed_store_fixture_for(
    game: ServerDefinitionKind,
    channel: ContentChannel,
) -> (
    tempfile::TempDir,
    tempfile::TempDir,
    ServerStore,
    ServerInstanceId,
    StoreClaim,
) {
    let data = tempfile::tempdir().unwrap();
    let install = tempfile::tempdir().unwrap();
    let store = fixture_store_for(
        data.path(),
        match game {
            ServerDefinitionKind::Arma3 => "arma3",
            ServerDefinitionKind::Reforger => "reforger",
        },
    );
    let claim = claim_store(&store);
    let draft = store
        .plan(DraftChange::Add {
            name: "ServerAlpha".into(),
        })
        .unwrap();
    let instance = draft.instance.clone();
    store.apply(draft).unwrap();
    create_native_install(install.path(), game);
    let inspected = store.inspect_installation(install.path()).unwrap();
    let files = content::inventory_tree(install.path(), MANIFEST_FILE, &|| false).unwrap();
    let manifest = ManagedManifest {
        schema_version: 1,
        game: inspected.game,
        platform: inspected.platform,
        app_id: super::super::dedicated_app(inspected.game, channel).unwrap(),
        channel,
        branch: "public".into(),
        build_id: Some(1),
        change_number: Some(1),
        depots: vec![ManagedDepot {
            depot_id: 233_781,
            manifest_gid: 10,
        }],
        files: files.len() as u64,
        bytes: files.iter().map(|file| file.bytes).sum(),
        inventory_blake3: inventory_hash(&files),
        executable: ManagedExecutable {
            path: inspected.executable.clone(),
            bytes: inspected.evidence.bytes,
            blake3: inspected.evidence.blake3.clone(),
        },
    };
    fs::write(
        install.path().join(MANIFEST_FILE),
        serde_json::to_vec(&manifest).unwrap(),
    )
    .unwrap();
    let managed = store.inspect_installation(install.path()).unwrap();
    assert_eq!(managed.ownership, InstallationOwnership::Managed);
    store
        .apply(
            store
                .plan(DraftChange::Adopt {
                    id: instance.id.clone(),
                    expected_revision: instance.revision,
                    installation: managed,
                })
                .unwrap(),
        )
        .unwrap();
    (data, install, store, instance.id, claim)
}

fn valid_install_plan(
    store: &ServerStore,
    id: &ServerInstanceId,
    destination: PathBuf,
) -> NativeInstallPlan {
    let instance = store.show(id).unwrap();
    let mut plan = NativeInstallPlan {
        context: store.context().clone(),
        instance_id: id.clone(),
        previous_installation: instance
            .installation_id
            .as_ref()
            .map(|installation| installation.as_str().to_owned()),
        instance_revision: instance.revision,
        previous_physical_key: Some(
            InstallationLocks::capture(&store.load().unwrap().installations[0].directory)
                .unwrap()
                .coordination_key()
                .unwrap(),
        ),
        catalog_fingerprint: store
            .read()
            .unwrap()
            .1
            .map(|hash| hash.to_hex().to_string()),
        maintenance: None,
        game: ServerDefinitionKind::Arma3,
        platform: ServerPlatform::current().unwrap(),
        app_id: 233_780,
        channel: ContentChannel::Stable,
        branch: "public".into(),
        account: None,
        directory: destination,
        review: ContentReview {
            selection: DepotSelection {
                app_id: 233_780,
                branch: "public".into(),
                build_id: Some(1),
                platform: steam_platform(ServerPlatform::current().unwrap()),
                depots: Vec::new(),
                skipped: Vec::new(),
            },
            change_number: None,
            entitled: true,
        },
        required_bytes: 0,
        available_bytes: None,
        interrupted_staging: Vec::new(),
        blockers: Vec::new(),
        fingerprint: String::new(),
        writes_performed: false,
        database_opened: false,
    };
    plan.fingerprint = super::super::plan_fingerprint(&plan);
    plan
}

#[test]
fn managed_inventory_validation_detects_missing_damaged_and_added_files_without_writes() {
    isolated_claimed_test(
        concat!(
            module_path!(),
            "::managed_inventory_validation_detects_missing_damaged_and_added_files_without_writes"
        ),
        || {
            let (_data, install, store, id, _claim) = managed_store_fixture();
            let valid = store
                .validate_native_installation(&id, &ContentStageControl::default())
                .unwrap();
            assert!(valid.valid, "{valid:?}");
            assert_eq!(valid.files, 1);
            assert!(valid.inventory_blake3.is_some());
            assert!(!valid.writes_performed && !valid.database_opened);

            fs::write(install.path().join("unknown-file.bin"), b"unknown").unwrap();
            let added = store
                .validate_native_installation(&id, &ContentStageControl::default())
                .unwrap();
            assert!(!added.valid);
            assert_eq!(added.files, 2);
            assert!(install.path().join("unknown-file.bin").exists());
            fs::remove_file(install.path().join("unknown-file.bin")).unwrap();

            let executable = install.path().join(executable_name(
                ServerDefinitionKind::Arma3,
                ServerPlatform::current().unwrap(),
            ));
            let original = fs::read(&executable).unwrap();
            fs::remove_file(&executable).unwrap();
            assert!(
                !store
                    .validate_native_installation(&id, &ContentStageControl::default())
                    .unwrap()
                    .valid
            );
            fs::write(&executable, &original).unwrap();
            let mut damaged = original.clone();
            *damaged.last_mut().unwrap() ^= 1;
            fs::write(&executable, damaged).unwrap();
            assert!(
                !store
                    .validate_native_installation(&id, &ContentStageControl::default())
                    .unwrap()
                    .valid
            );
        },
    );
}

#[test]
fn changed_or_malformed_receipts_are_refused_and_adopted_installations_cannot_validate() {
    isolated_claimed_test(
        concat!(
            module_path!(),
            "::changed_or_malformed_receipts_are_refused_and_adopted_installations_cannot_validate"
        ),
        || {
            let (_data, install, store, id, _claim) = managed_store_fixture();
            let receipt = install.path().join(MANIFEST_FILE);
            let mut manifest: ManagedManifest =
                serde_json::from_slice(&fs::read(&receipt).unwrap()).unwrap();
            manifest.inventory_blake3 = "f".repeat(64);
            fs::write(&receipt, serde_json::to_vec(&manifest).unwrap()).unwrap();
            assert!(
                !store
                    .validate_native_installation(&id, &ContentStageControl::default())
                    .unwrap()
                    .valid
            );
            fs::write(&receipt, b"{").unwrap();
            assert!(matches!(
                store.validate_native_installation(&id, &ContentStageControl::default()),
                Err(ServerStoreError::Corrupt)
            ));

            let adopted_dir = tempfile::tempdir().unwrap();
            create_native_install(adopted_dir.path(), ServerDefinitionKind::Arma3);
            let inspected = store.inspect_installation(adopted_dir.path()).unwrap();
            assert_eq!(inspected.ownership, InstallationOwnership::Adopted);
            let draft = store
                .plan(DraftChange::Add {
                    name: "AdoptedServer".into(),
                })
                .unwrap();
            let adopted_id = draft.instance.id.clone();
            store.apply(draft).unwrap();
            let current = store.show(&adopted_id).unwrap();
            store
                .apply(
                    store
                        .plan(DraftChange::Adopt {
                            id: adopted_id.clone(),
                            expected_revision: current.revision,
                            installation: inspected,
                        })
                        .unwrap(),
                )
                .unwrap();
            assert!(matches!(
                store.validate_native_installation(&adopted_id, &ContentStageControl::default()),
                Err(ServerStoreError::Invalid(_))
            ));
        },
    );
}

#[test]
fn managed_inventory_validation_honors_cancellation() {
    isolated_claimed_test(
        concat!(
            module_path!(),
            "::managed_inventory_validation_honors_cancellation"
        ),
        || {
            let (_data, _install, store, id, _claim) = managed_store_fixture();
            let control = ContentStageControl::default();
            control.cancel();
            assert!(matches!(
                store.validate_native_installation(&id, &control),
                Err(ServerStoreError::Cancelled)
            ));
        },
    );
}

#[test]
fn native_apply_rejects_a_stale_instance_revision_before_authentication() {
    isolated_claimed_test(
        concat!(
            module_path!(),
            "::native_apply_rejects_a_stale_instance_revision_before_authentication"
        ),
        || {
            let (_data, _install, store, id, _claim) = managed_store_fixture();
            let instance = store.show(&id).unwrap();
            let installation_id = instance
                .installation_id
                .as_ref()
                .unwrap()
                .as_str()
                .to_owned();
            let review = ContentReview {
                selection: DepotSelection {
                    app_id: 233_780,
                    branch: "public".into(),
                    build_id: Some(1),
                    platform: Platform::WindowsX64,
                    depots: Vec::new(),
                    skipped: Vec::new(),
                },
                change_number: None,
                entitled: true,
            };
            let mut plan = NativeInstallPlan {
                context: store.context().clone(),
                instance_id: id,
                previous_installation: Some(installation_id),
                previous_physical_key: None,
                instance_revision: instance.revision - 1,
                catalog_fingerprint: store
                    .read()
                    .unwrap()
                    .1
                    .map(|hash| hash.to_hex().to_string()),
                maintenance: None,
                game: ServerDefinitionKind::Arma3,
                platform: ServerPlatform::current().unwrap(),
                app_id: 233_780,
                channel: ContentChannel::Stable,
                branch: "public".into(),
                account: None,
                directory: PathBuf::from("/unused-destination"),
                review,
                required_bytes: 0,
                available_bytes: None,
                interrupted_staging: Vec::new(),
                blockers: Vec::new(),
                fingerprint: String::new(),
                writes_performed: false,
                database_opened: false,
            };
            plan.fingerprint = super::super::plan_fingerprint(&plan);
            assert!(matches!(
                store.apply_native_install(&plan, &ContentStageControl::default()),
                Err(ServerStoreError::StaleRevision)
            ));
        },
    );
}

#[test]
fn native_apply_is_blocked_by_an_active_runtime_lease_before_authentication() {
    isolated_claimed_test(
        concat!(
            module_path!(),
            "::native_apply_is_blocked_by_an_active_runtime_lease_before_authentication"
        ),
        || {
            let (_data, install, store, id, _claim) = managed_store_fixture();
            let original_executable = fs::read(install.path().join(executable_name(
                ServerDefinitionKind::Arma3,
                ServerPlatform::current().unwrap(),
            )))
            .unwrap();
            let destination_parent = tempfile::tempdir().unwrap();
            let destination = destination_parent.path().join("NewServer");
            let mut plan = valid_install_plan(&store, &id, destination.clone());
            plan.account = Some("unprepared".into());
            plan.fingerprint = super::super::plan_fingerprint(&plan);

            let locks =
                foxy_supervisor::leases::InstallationLocks::capture(install.path()).unwrap();
            let admission = locks.try_admit().unwrap();
            let _runtime = admission.try_runtime().unwrap();
            let error = store
                .apply_native_install(&plan, &ContentStageControl::default())
                .unwrap_err();
            assert!(matches!(
                error,
                ServerStoreError::Invalid(message) if message.contains("active operation")
            ));
            drop(admission);
            assert!(matches!(
                store.apply_native_install(&plan, &ContentStageControl::default()),
                Err(ServerStoreError::Invalid(message)) if message.contains("active operation")
            ));
            assert!(!destination.exists());
            assert_eq!(
                fs::read(install.path().join(executable_name(
                    ServerDefinitionKind::Arma3,
                    ServerPlatform::current().unwrap(),
                )))
                .unwrap(),
                original_executable
            );
        },
    );
}

#[test]
fn maintenance_revalidation_rejects_changed_physical_key_and_receipt() {
    isolated_claimed_test(
        concat!(
            module_path!(),
            "::maintenance_revalidation_rejects_changed_physical_key_and_receipt"
        ),
        || {
            let (_data, install, store, id, _claim) = managed_store_fixture();
            let previous = store.load().unwrap().installations.remove(0);
            let locks =
                foxy_supervisor::leases::InstallationLocks::capture(install.path()).unwrap();
            let expected_key = locks.coordination_key().unwrap();
            let receipt_path = install.path().join(MANIFEST_FILE);
            let original_receipt = fs::read(&receipt_path).unwrap();
            let receipt_fingerprint = blake3::hash(&original_receipt).to_hex().to_string();
            let consumers = store.installation_consumers(&previous.id).unwrap();
            let mut plan = valid_install_plan(
                &store,
                &id,
                install.path().parent().unwrap().join("NewServer"),
            );
            plan.maintenance = Some(NativeMaintenanceReview {
                action: NativeMaintenanceAction::Repair,
                previous: previous.clone(),
                physical_key: "changed-physical-key".into(),
                manifest_fingerprint: receipt_fingerprint.clone(),
                previous_build_id: Some(1),
                previous_depots: vec![(233_781, "10".into())],
                update_available: false,
                retained_peer_instances: Vec::new(),
                consumers,
            });
            assert!(matches!(
                store.revalidate_maintenance(&plan, Some(&locks)),
                Err(ServerStoreError::StaleRevision)
            ));

            plan.maintenance.as_mut().unwrap().physical_key = expected_key;
            let mut changed_receipt: ManagedManifest =
                serde_json::from_slice(&original_receipt).unwrap();
            changed_receipt.change_number = Some(2);
            fs::write(&receipt_path, serde_json::to_vec(&changed_receipt).unwrap()).unwrap();
            assert!(matches!(
                store.revalidate_maintenance(&plan, Some(&locks)),
                Err(ServerStoreError::StaleRevision)
            ));
        },
    );
}

#[test]
fn plain_install_cannot_rebind_a_replaced_physical_directory_after_review() {
    isolated_claimed_test(
        concat!(
            module_path!(),
            "::plain_install_cannot_rebind_a_replaced_physical_directory_after_review"
        ),
        || {
            let (_data, install, store, id, _claim) = managed_store_fixture();
            let replacement = tempfile::tempdir().unwrap();
            let destination = replacement.path().join("Successor");
            let mut plan = valid_install_plan(&store, &id, destination.clone());
            plan.account = Some("unprepared".into());
            plan.fingerprint = super::super::plan_fingerprint(&plan);
            let original = replacement.path().join("RetainedOriginal");
            fs::rename(install.path(), &original).unwrap();
            fs::create_dir(install.path()).unwrap();
            for entry in fs::read_dir(&original).unwrap() {
                let entry = entry.unwrap();
                assert!(entry.file_type().unwrap().is_file());
                fs::copy(entry.path(), install.path().join(entry.file_name())).unwrap();
            }
            let before = fs::read(store.path()).unwrap();
            let installation = store.load().unwrap().installations.remove(0);
            installation.verify().unwrap();
            assert!(matches!(
                store.apply_native_install(&plan, &ContentStageControl::default()),
                Err(ServerStoreError::StaleRevision)
            ));
            assert_eq!(fs::read(store.path()).unwrap(), before);
            assert_eq!(
                fs::read(original.join(MANIFEST_FILE)).unwrap(),
                fs::read(install.path().join(MANIFEST_FILE)).unwrap()
            );
            assert!(!destination.exists());
        },
    );
}

#[test]
fn destination_inside_an_installation_is_refused_across_configuration_roots() {
    isolated_claimed_test(
        concat!(
            module_path!(),
            "::destination_inside_an_installation_is_refused_across_configuration_roots"
        ),
        || {
            let (_data, install, _store, _id, _claim) = managed_store_fixture();
            let other_data = tempfile::tempdir().unwrap();
            let other_store = fixture_store(other_data.path());
            let executable = install.path().join(executable_name(
                ServerDefinitionKind::Arma3,
                ServerPlatform::current().unwrap(),
            ));
            let original_executable = fs::read(&executable).unwrap();
            let nested_destination = install.path().join("NestedServer");
            assert!(matches!(
                super::super::validate_destination(&other_store, &nested_destination),
                Err(ServerStoreError::Invalid(_))
            ));
            assert!(!nested_destination.exists());

            let receipt = install.path().join(MANIFEST_FILE);
            let original_receipt = fs::read(&receipt).unwrap();
            fs::remove_file(&receipt).unwrap();
            let executable_only_destination = install.path().join("NestedServerTwo");
            assert!(matches!(
                super::super::validate_destination(&other_store, &executable_only_destination),
                Err(ServerStoreError::Invalid(_))
            ));
            assert!(!executable_only_destination.exists());
            fs::write(receipt, original_receipt).unwrap();
            assert_eq!(fs::read(executable).unwrap(), original_executable);
        },
    );
}

#[test]
fn maintenance_change_detection_includes_depot_gid_when_build_id_matches() {
    let mut manifest = ManagedManifest {
        schema_version: 1,
        game: ServerDefinitionKind::Arma3,
        platform: ServerPlatform::current().unwrap(),
        app_id: 233_780,
        channel: ContentChannel::Stable,
        branch: "public".into(),
        build_id: Some(1),
        change_number: None,
        depots: vec![ManagedDepot {
            depot_id: 233_781,
            manifest_gid: 10,
        }],
        files: 0,
        bytes: 0,
        inventory_blake3: "0".repeat(64),
        executable: ManagedExecutable {
            path: "server".into(),
            bytes: 0,
            blake3: "0".repeat(64),
        },
    };
    let mut selection = DepotSelection {
        app_id: manifest.app_id,
        branch: manifest.branch.clone(),
        build_id: manifest.build_id,
        platform: steam_platform(manifest.platform),
        depots: vec![foxy_steam::content::SelectedDepot {
            depot_id: manifest.depots[0].depot_id,
            name: None,
            manifest_gid: 11,
            size: None,
            download: None,
        }],
        skipped: Vec::new(),
    };
    assert!(changed(&manifest, &selection));
    manifest.depots[0].manifest_gid = 11;
    assert!(!changed(&manifest, &selection));
    selection.app_id = 1_890_870;
    assert!(changed(&manifest, &selection));
    selection.app_id = manifest.app_id;
    selection.platform = match manifest.platform {
        ServerPlatform::WindowsX64 => Platform::LinuxX64,
        ServerPlatform::LinuxX64 => Platform::WindowsX64,
    };
    assert!(changed(&manifest, &selection));
}

#[test]
fn reforger_channels_refuse_cross_channel_maintenance_before_provider_or_destination_writes() {
    isolated_claimed_test(
        concat!(
            module_path!(),
            "::reforger_channels_refuse_cross_channel_maintenance_before_provider_or_destination_writes"
        ),
        || {
            for channel in [ContentChannel::Stable, ContentChannel::Experimental] {
                let (_data, install, store, id, _claim) =
                    managed_store_fixture_for(ServerDefinitionKind::Reforger, channel);
                let installation = store.managed_for(&id).unwrap();
                let receipt_before = fs::read(install.path().join(MANIFEST_FILE)).unwrap();
                let receipt = manifest(&installation).unwrap().0;
                assert_eq!(receipt.channel, channel);
                assert_eq!(
                    receipt.app_id,
                    if channel == ContentChannel::Stable {
                        1_874_900
                    } else {
                        1_890_870
                    }
                );
                assert!(
                    store
                        .validate_native_installation(&id, &ContentStageControl::default())
                        .unwrap()
                        .valid
                );
                let destination = install
                    .path()
                    .parent()
                    .unwrap()
                    .join(format!("unused-{}", id.as_str()));
                let other = if channel == ContentChannel::Stable {
                    ContentChannel::Experimental
                } else {
                    ContentChannel::Stable
                };
                for action in [
                    NativeMaintenanceAction::Update,
                    NativeMaintenanceAction::Repair,
                ] {
                    let error = store
                        .plan_native_maintenance(
                            NativeInstallRequest {
                                instance_id: id.clone(),
                                directory: destination.clone(),
                                branch: "public".into(),
                                channel: Some(other),
                                account: None,
                            },
                            action,
                            &ContentStageControl::default(),
                        )
                        .unwrap_err();
                    assert!(error.to_string().contains("retain the installed channel"));
                    assert!(!destination.exists());
                    assert_eq!(
                        fs::read(install.path().join(MANIFEST_FILE)).unwrap(),
                        receipt_before
                    );
                }
            }
        },
    );
}
