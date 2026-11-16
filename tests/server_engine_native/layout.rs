use super::*;

pub(super) fn observe() -> io::Result<()> {
    if !foxy_supervisor::process::support().available {
        return Err(io::Error::other("Native containment unavailable"));
    }
    let source = input_directory("FOXY_ARMA_NATIVE_SOURCE")?;
    let scratch = input_directory("FOXY_ARMA_NATIVE_SCRATCH")?;
    if source.starts_with(&scratch) || scratch.starts_with(&source) {
        return Err(io::Error::other("Source and scratch must be separate"));
    }
    let shared = match std::env::var("FOXY_ARMA_NATIVE_LAYOUT").as_deref() {
        Ok("shared_222") => true,
        Ok("isolated_legacy") => false,
        _ => return Err(io::Error::other("Choose shared_222 or isolated_legacy")),
    };
    let public_keys = std::env::var_os("FOXY_ARMA_NATIVE_PUBLIC_KEYS")
        .map(|_| input_directory("FOXY_ARMA_NATIVE_PUBLIC_KEYS"))
        .transpose()?;
    let prepared = if std::env::var_os("FOXY_ARMA_NATIVE_PREPARED").is_some() {
        let path = input_directory("FOXY_ARMA_NATIVE_PREPARED")?;
        if !shared || !path.starts_with(&scratch) || path == scratch {
            return Err(io::Error::other(
                "Prepared copies must be private shared-layout inputs",
            ));
        }
        Some(path)
    } else {
        None
    };
    let root = tempfile::Builder::new()
        .prefix("arma-layout-")
        .tempdir_in(scratch)?
        .keep();
    println!(
        "Private evidence directory: {}",
        root.file_name().unwrap().to_string_lossy()
    );
    let source_locks = InstallationLocks::capture_in(&source, &root.join("source-locks"))?;
    let _source_read = source_locks.try_admit()?.try_runtime()?;
    let reuse = prepared.is_some();
    let payload = prepared.unwrap_or_else(|| root.join("payload-alpha"));
    let prepared_locks = if reuse {
        Some(InstallationLocks::capture_in(
            &payload,
            &payload.parent().unwrap().join("runtime-locks"),
        )?)
    } else {
        None
    };
    let maintenance = prepared_locks
        .as_ref()
        .map(|locks| {
            locks
                .try_admit()?
                .try_maintenance()
                .map_err(|(_, error)| error)
        })
        .transpose()?;
    let source_inventory = copy_payload(&source, &payload, reuse)?;
    drop(maintenance);
    let second_payload = if shared {
        payload.clone()
    } else {
        let second = root.join("payload-beta");
        copy_payload(&source, &second, false)?;
        second
    };
    let mut alpha =
        Instance::prepare_with_keys(&root, &payload, "alpha", shared, public_keys.as_deref())?;
    let mut beta = Instance::prepare_with_keys(
        &root,
        &second_payload,
        "beta",
        shared,
        public_keys.as_deref(),
    )?;
    let registry = payload.parent().unwrap().join("runtime-locks");
    let alpha_locks = InstallationLocks::capture_in(&payload, &registry)?;
    let beta_locks = InstallationLocks::capture_in(&second_payload, &registry)?;
    let mut sessions = vec![alpha.start(&alpha_locks)?, beta.start(&beta_locks)?];
    let deadline = Instant::now() + Duration::from_secs(180);
    let observation = loop {
        let observations = [alpha.observation()?, beta.observation()?];
        if observations.iter().all(|item| item.ready) {
            break Ok(observations);
        }
        if sessions
            .iter_mut()
            .map(ForegroundSession::observe)
            .collect::<io::Result<Vec<_>>>()?
            .iter()
            .any(Option::is_some)
        {
            break Err(io::Error::other("Engine exited before bootstrap readiness"));
        }
        if Instant::now() >= deadline {
            break Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "Engine bootstrap readiness was not observed",
            ));
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    let mut key_isolation = false;
    if public_keys.is_some() && observation.is_ok() {
        let deadline = Instant::now() + Duration::from_secs(45);
        loop {
            let signatures = [
                query_keys::query_keys(alpha.port + 1, &alpha.hostname),
                query_keys::query_keys(beta.port + 1, &beta.hostname),
            ];
            key_isolation = signatures[0]
                .as_ref()
                .is_some_and(|keys| key_destinations::isolated(keys, "alpha"))
                && signatures[1]
                    .as_ref()
                    .is_some_and(|keys| key_destinations::isolated(keys, "beta"));
            if key_isolation || Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    let queries = [alpha.query_ready(), beta.query_ready()];
    let proofs = [alpha.proof()?, beta.proof()?];
    for session in &mut sessions {
        if session.force_stop(Duration::from_secs(5))?.is_none() {
            return Err(io::Error::other("Contained engine tree still owned"));
        }
    }
    let mut persistence_verified = false;
    let restart = if observation.is_ok() {
        std::thread::sleep(Duration::from_millis(1100));
        let mut restarted = vec![alpha.start(&alpha_locks)?, beta.start(&beta_locks)?];
        let deadline = Instant::now() + Duration::from_secs(180);
        let outcome = loop {
            let observations = [alpha.observation()?, beta.observation()?];
            let proofs = [alpha.proof()?, beta.proof()?];
            if observations.iter().all(|item| item.ready)
                && proofs
                    .iter()
                    .all(|proof| proof.profile_readback && proof.mission_readback)
            {
                persistence_verified = true;
                break Ok(());
            }
            if restarted
                .iter_mut()
                .map(ForegroundSession::observe)
                .collect::<io::Result<Vec<_>>>()?
                .iter()
                .any(Option::is_some)
            {
                break Err(io::Error::other("Engine exited before namespace readback"));
            }
            if Instant::now() >= deadline {
                break Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "Namespace readback was not observed after restart",
                ));
            }
            std::thread::sleep(Duration::from_millis(100));
        };
        for session in &mut restarted {
            if session.force_stop(Duration::from_secs(5))?.is_none() {
                return Err(io::Error::other("Restarted engine tree still owned"));
            }
        }
        Some(outcome)
    } else {
        None
    };
    verify_inventory(&source, &source_inventory)?;
    verify_inventory(&payload, &source_inventory)?;
    if !shared {
        verify_inventory(&second_payload, &source_inventory)?;
    }
    let observations = [alpha.observation()?, beta.observation()?];
    let report = serde_json::json!({
        "schema_version": 1,
        "layout_request": if shared { "shared_222" } else { "isolated_legacy" },
        "engine_version": observations[0].version,
        "engine_reported_build": observations[0].build,
        "engine_reported_type": observations[0].distribution,
        "bootstrap_observed": observation.is_ok(),
        "a2s_query_observed": queries,
        "mission_markers_observed": proofs.iter().all(|proof| proof.mission_seen),
        "server_content_markers_isolated": proofs.iter().all(|proof| proof.content_isolated),
        "profile_namespace_readback": proofs.iter().all(|proof| proof.profile_readback),
        "mission_namespace_readback": proofs.iter().all(|proof| proof.mission_readback),
        "namespace_persistence_verified_after_restart": persistence_verified,
        "separate_profile_reports": observations.iter().all(|item| !item.version.is_empty()),
        "selected_source_files_unchanged": true,
        "selected_copied_files_unchanged": true,
        "stop_outcome": "forced_tree_exit_save_uncertain",
        "mission_isolation_verified": false,
        "key_isolation_verified": false,
        "engine_advertised_key_destinations_isolated": key_isolation,
        "content_isolation_verified": false,
        "save_isolation_verified": false,
        "layout_accepted": false,
        "lifecycle_accepted": false
    });
    fs::write(
        root.join("observation.json"),
        serde_json::to_vec_pretty(&report)?,
    )?;
    println!("{report}");
    let observations = observation?;
    if public_keys.is_some() && !key_isolation {
        return Err(io::Error::other(
            "Engine-advertised key isolation was not observed",
        ));
    }
    if let Some(restart) = restart {
        restart?;
    }
    assert_eq!(observations[0].version, observations[1].version);
    if shared {
        assert!(observations[0].version.starts_with("2.22."));
    }
    Ok(())
}
