use super::*;
use std::process::{Command, Stdio};

fn tool(
    tools: &Path,
    name: &str,
    directory: &Path,
    arguments: &[&std::ffi::OsStr],
) -> io::Result<()> {
    ordinary(&tools.join(name), false)?;
    let mut child = Command::new(tools.join(name))
        .current_dir(directory)
        .args(arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Some(status) = child.try_wait()? {
            return if status.success() {
                Ok(())
            } else {
                Err(io::Error::other("Native signing tool failed"))
            };
        }
        if Instant::now() >= deadline {
            child.kill()?;
            child.wait()?;
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "Native signing tool timed out",
            ));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

pub(crate) fn observe() -> io::Result<()> {
    use std::ffi::OsStr;
    let source = input_directory("FOXY_ARMA_NATIVE_SOURCE")?;
    let scratch = input_directory("FOXY_ARMA_NATIVE_SCRATCH")?;
    let tools = input_directory("FOXY_ARMA_NATIVE_SIGNING_TOOLS")?;
    if source.starts_with(&scratch) || scratch.starts_with(&source) {
        return Err(io::Error::other("Source and scratch must be separate"));
    }
    let payload = input_directory("FOXY_ARMA_NATIVE_PREPARED")?;
    if !payload.starts_with(&scratch) || payload == scratch {
        return Err(io::Error::other(
            "Key probe requires a private prepared copy",
        ));
    }
    let root = tempfile::Builder::new()
        .prefix("arma-keys-")
        .tempdir_in(&scratch)?
        .keep();
    println!(
        "Private key evidence directory: {}",
        root.file_name().unwrap().to_string_lossy()
    );
    let source_locks = InstallationLocks::capture_in(&source, &root.join("source-locks"))?;
    let _source_read = source_locks.try_admit()?.try_runtime()?;
    let registry = payload.parent().unwrap().join("runtime-locks");
    let locks = InstallationLocks::capture_in(&payload, &registry)?;
    let maintenance = locks
        .try_admit()?
        .try_maintenance()
        .map_err(|(_, error)| error)?;
    let inventory = copy_payload(&source, &payload, true)?;
    let client = "arma3_x64.exe";
    if ordinary(&source.join(client), false)?.len() > 512 * 1024 * 1024 {
        return Err(io::Error::other(
            "Client fixture exceeds bounded executable size",
        ));
    }
    let client_size = ordinary(&source.join(client), false)?.len();
    match fs::symlink_metadata(payload.join(client)) {
        Ok(_) => {
            ordinary(&payload.join(client), false)?;
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let mut client_output = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(payload.join(client))?;
            let copied = io::copy(
                &mut fs::File::open(source.join(client))?.take(client_size + 1),
                &mut client_output,
            )?;
            client_output.sync_all()?;
            if copied != client_size {
                return Err(io::Error::other("Client source changed during copy"));
            }
        }
        Err(error) => return Err(error),
    }
    if ordinary(&payload.join(client), false)?.len() != client_size {
        return Err(io::Error::other("Client fixture size differs"));
    }
    let client_hash = blake3::hash(&fs::read(source.join(client))?);
    if blake3::hash(&fs::read(payload.join(client))?) != client_hash {
        return Err(io::Error::other("Client copy verification failed"));
    }
    let password = format!("{:016x}", rand::random::<u64>());
    let mut alpha = Instance::prepare_client_joinable(&root, &payload, "alpha", &password)?;
    let mut beta = Instance::prepare_client_joinable(&root, &payload, "beta", &password)?;
    for (name, key) in [("alpha", "FoxyNativeAlpha"), ("beta", "FoxyNativeBeta")] {
        tool(&tools, "DSCreateKey.exe", &root, &[OsStr::new(key)])?;
        let public = root.join(format!("{key}.bikey"));
        ordinary(&public, false)?;
        let destination = payload
            .join("private-instances")
            .join(root.file_name().unwrap())
            .join(name)
            .join("keys")
            .join(format!("{key}.bikey"));
        fs::copy(public, destination)?;
    }
    let missions = root.join("unused-mission");
    fs::create_dir(&missions)?;
    let client_mod = root.join("client-content");
    mission::write(&missions, &client_mod, "alpha", "FoxyNativeClient")?;
    let private_key = root.join("FoxyNativeAlpha.biprivatekey");
    let pbo = client_mod.join("addons/foxy_native_alpha.pbo");
    tool(
        &tools,
        "DSSignFile.exe",
        &root,
        &[private_key.as_os_str(), pbo.as_os_str()],
    )?;
    ordinary(
        &pbo.with_file_name("foxy_native_alpha.pbo.FoxyNativeAlpha.bisign"),
        false,
    )?;
    let public_key = root.join("FoxyNativeAlpha.bikey");
    tool(
        &tools,
        "DSCheckSignatures.exe",
        &root,
        &[public_key.as_os_str(), pbo.parent().unwrap().as_os_str()],
    )?;
    drop(maintenance);
    let mut sessions = vec![alpha.start(&locks)?, beta.start(&locks)?];
    let deadline = Instant::now() + Duration::from_secs(120);
    while !alpha.observation()?.ready || !beta.observation()?.ready {
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "Key servers did not start their missions",
            ));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let mut outcomes = vec![];
    let mut refusals = vec![];
    for instance in [&alpha, &beta] {
        let client_root = root.join(format!("client-{}", instance.name));
        fs::create_dir(&client_root)?;
        let profiles = client_root.join("profiles");
        fs::create_dir(&profiles)?;
        if let Some(seed) = std::env::var_os("FOXY_ARMA_NATIVE_CLIENT_PROFILE_SEED") {
            let seed = PathBuf::from(seed);
            if !seed.is_absolute()
                || !seed.canonicalize()?.starts_with(scratch.canonicalize()?)
                || seed.file_name()
                    != Some(std::ffi::OsStr::new("FoxyNativeClient.vars.Arma3Profile"))
                || ordinary(&seed, false)?.len() > 1024 * 1024
            {
                return Err(io::Error::other(
                    "Client profile seed must be an ordinary private fixture",
                ));
            }
            let user = profiles.join("Users/FoxyNativeClient");
            fs::create_dir_all(&user)?;
            let bytes = fs::read(seed)?;
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(user.join("FoxyNativeClient.vars.Arma3Profile"))?;
            file.write_all(&bytes)?;
            file.sync_all()?;
        }
        let mut client_session = ForegroundSession::spawn(
            SessionIdentity {
                owner_id: "native_key_probe".into(),
                owner_generation: 1,
                game_space_id: "native_probe".into(),
                instance_id: format!("client_{}", instance.name),
                operation_id: "key_admission".into(),
                run_generation: 1,
            },
            &CommandSpec {
                executable: payload.join(client),
                directory: payload.clone(),
                arguments: vec![
                    OsString::from("-connect=127.0.0.1"),
                    OsString::from(format!("-password={password}")),
                    OsString::from(format!("-port={}", instance.port)),
                    OsString::from(format!("-profiles={}", profiles.display())),
                    OsString::from("-name=FoxyNativeClient"),
                    OsString::from(format!("-mod={}", client_mod.display())),
                    OsString::from("-noSplash"),
                    OsString::from("-noSound"),
                    OsString::from("-skipIntro"),
                    OsString::from("-noPause"),
                    OsString::from("-world=empty"),
                    OsString::from("-window"),
                ],
                environment: vec![],
            },
            locks.try_admit()?,
            &AtomicBool::new(false),
            || Ok(()),
        )?;
        let deadline = Instant::now() + Duration::from_secs(120);
        let mut admitted = false;
        while Instant::now() < deadline {
            admitted |= instance.proof()?.regular_client_present;
            if admitted || client_session.observe()?.is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        if client_session.force_stop(Duration::from_secs(5))?.is_none() {
            return Err(io::Error::other("Key client tree still owned"));
        }
        let mut client_text = String::new();
        for entry in fs::read_dir(&profiles)?.take(128) {
            let entry = entry?;
            if entry
                .path()
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("rpt"))
            {
                ordinary(&entry.path(), false)?;
                fs::File::open(entry.path())?
                    .take(16 * 1024 * 1024)
                    .read_to_string(&mut client_text)?;
                if client_text.len() > 16 * 1024 * 1024 {
                    return Err(io::Error::other("Key client report exceeds bounds"));
                }
            }
        }
        let refused = proof::explicit_fixture_key_refusal(&client_text);
        outcomes.push(admitted);
        refusals.push(refused);
        println!(
            "Regular client admission observed for {}: {}; explicit key refusal: {}",
            instance.name, admitted, refused
        );
    }
    for session in &mut sessions {
        if session.force_stop(Duration::from_secs(5))?.is_none() {
            return Err(io::Error::other("Key server tree still owned"));
        }
    }
    verify_inventory(&source, &inventory)?;
    verify_inventory(&payload, &inventory)?;
    if blake3::hash(&fs::read(source.join(client))?) != client_hash
        || blake3::hash(&fs::read(payload.join(client))?) != client_hash
    {
        return Err(io::Error::other("Client fixture changed"));
    }
    let accepted = outcomes == [true, false] && refusals == [false, true];
    let report = serde_json::json!({ "schema_version":1,"matching_key_admitted":outcomes[0],"different_key_admitted":outcomes[1],"explicit_key_refusals":refusals,"key_policy_accepted":accepted,"selected_source_files_unchanged":true,"stop_outcome":"forced_tree_exit_save_uncertain" });
    fs::write(
        root.join("key-observation.json"),
        serde_json::to_vec_pretty(&report)?,
    )?;
    println!("{report}");
    if !accepted {
        return Err(io::Error::other(
            "Matching accept and different-key refusal were not both observed",
        ));
    }
    Ok(())
}
