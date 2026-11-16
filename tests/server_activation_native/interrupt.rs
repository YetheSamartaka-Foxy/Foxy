use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use foxy_supervisor::leases::InstallationLocks;
use serde_json::Value;

use super::preparation::{PreparationRevision, Prepared, prepare};
use super::{
    assert_ready, invoke, run_active_engine, snapshot_selected_payload, snapshot_tree, string,
};
#[path = "interrupt_process.rs"]
mod process;
use process::{Controller, ParentOwner, cli};

#[derive(Clone, Copy)]
enum Boundary {
    Intent,
    Journal,
    Pointer,
}

pub(super) struct InterruptContext<'a> {
    pub config: &'a Path,
    pub case: &'a Path,
    pub source: &'a Path,
    pub id: &'a str,
    pub predecessor: &'a Prepared,
    pub predecessor_engine: &'a Path,
    pub predecessor_inventory: &'a std::collections::BTreeMap<PathBuf, (u64, blake3::Hash)>,
    pub draft_revision: u64,
    pub configuration_generation: u64,
}

impl Boundary {
    fn label(self) -> &'static str {
        match self {
            Self::Intent => "intent_without_journal",
            Self::Journal => "journal_with_predecessor_pointer",
            Self::Pointer => "new_pointer_with_journal",
        }
    }

    fn matches(self, pending: bool, journal: bool, active_generation: u64) -> bool {
        if !pending {
            return false;
        }
        match self {
            Self::Intent => !journal && active_generation == 3,
            Self::Journal => journal && active_generation == 3,
            Self::Pointer => journal && active_generation == 4,
        }
    }

    fn expected_outcome(self) -> &'static str {
        if matches!(self, Self::Pointer) {
            "committed"
        } else {
            "aborted"
        }
    }
}

pub(super) fn run(context: InterruptContext<'_>) -> io::Result<()> {
    let InterruptContext {
        config,
        case,
        source,
        id,
        predecessor,
        predecessor_engine,
        predecessor_inventory,
        mut draft_revision,
        mut configuration_generation,
    } = context;
    let source_inventory = snapshot_selected_payload(source)?;
    let locks = InstallationLocks::capture(source)?;
    for (index, boundary) in [Boundary::Intent, Boundary::Journal, Boundary::Pointer]
        .into_iter()
        .enumerate()
    {
        let hostname = format!(
            "FoxyNativeInterrupt{}_{}",
            (b'C' + index as u8) as char,
            super::fresh_nonce()
        );
        (draft_revision, configuration_generation) = super::update_configuration(
            config,
            id,
            draft_revision,
            configuration_generation,
            &hostname,
        )?;
        let target = prepare(
            config,
            case,
            id,
            if index == 1 { "beta" } else { "alpha" },
            &hostname,
            PreparationRevision {
                deployment: 3 + index as u64,
                draft: draft_revision,
                configuration: configuration_generation,
            },
        )?;
        let selection = 4 + index as u64;
        let plan = invoke(
            config,
            &[
                "server",
                "deploy",
                "activation",
                "plan",
                id,
                &target.preparation,
                "--expected-deployment-generation",
                &selection.to_string(),
                "--expected-activation-generation",
                "3",
            ],
            "plan interrupted activation",
        )?;
        let fingerprint = string(
            &plan["data"]["plan"]["fingerprint"],
            "interrupt activation fingerprint",
        )?;
        let runtime_dir = PathBuf::from(string(
            &plan["data"]["plan"]["runtime_directory"],
            "interrupt runtime directory",
        )?);
        let generation_before = active_generation(config, id)?;
        assert_eq!(generation_before, 3);
        let mut owner = ParentOwner::start(config)?;
        let mut controller = Controller::start(config, id, &target, selection, 3, fingerprint)?;
        let observed = wait_boundary(
            &mut owner,
            &mut controller,
            &locks,
            &activation_root(config, id),
            boundary,
            Duration::from_secs(8 * 60),
        );
        if let Err(error) = observed {
            controller.kill_and_wait()?;
            owner.stop()?;
            return Err(io::Error::other(format!(
                "{} boundary was not observed: {error}",
                boundary.label()
            )));
        }
        eprintln!("Native owner interruption observed: {}", boundary.label());
        owner.kill_and_wait()?;
        let controller_exit = controller.wait_after_owner_loss()?;
        assert!(
            !controller_exit.success(),
            "controller should report an unavailable or uncertain owner result after owner termination"
        );
        let retained_paths = directory_names(
            runtime_dir
                .parent()
                .ok_or_else(|| io::Error::other("Missing runtime parent"))?,
        )?;
        let candidate_before_resolution = if runtime_dir.exists() {
            Some(snapshot_tree(&runtime_dir)?)
        } else {
            None
        };
        let mut recovery_owner = ParentOwner::start(config)?;
        let review = invoke(
            config,
            &["server", "deploy", "activation", "recovery-plan", id],
            "plan interrupted activation recovery",
        )?;
        let recovery = &review["data"]["review"];
        assert_eq!(recovery["expected_outcome"], boundary.expected_outcome());
        let resolved = invoke(
            config,
            &[
                "--yes",
                "server",
                "deploy",
                "activation",
                "resolve",
                id,
                "--expected-plan-fingerprint",
                string(&recovery["fingerprint"], "recovery fingerprint")?,
            ],
            "resolve interrupted activation",
        )?;
        assert_eq!(
            resolved["data"]["outcome"]["status"],
            boundary.expected_outcome()
        );
        recovery_owner.stop()?;
        assert_eq!(
            active_generation(config, id)?,
            if boundary.expected_outcome() == "committed" {
                4
            } else {
                3
            }
        );
        assert_eq!(
            activation_status(config, id)?["data"]["status"]["recovery_required"],
            false
        );
        assert_eq!(
            directory_names(
                runtime_dir
                    .parent()
                    .ok_or_else(|| io::Error::other("Missing runtime parent"))?
            )?,
            retained_paths
        );
        let candidate_after_resolution = if runtime_dir.exists() {
            Some(snapshot_tree(&runtime_dir)?)
        } else {
            None
        };
        assert_eq!(candidate_after_resolution, candidate_before_resolution);
        assert_eq!(snapshot_selected_payload(source)?, source_inventory);
        assert_eq!(
            snapshot_selected_payload(predecessor_engine)?,
            *predecessor_inventory
        );
        let maintenance = locks.inspect_maintenance()?;
        assert!(maintenance.pending.is_none());
        assert!(maintenance.receipts.iter().any(|receipt| {
            format!("{:?}", receipt.outcome).eq_ignore_ascii_case(boundary.expected_outcome())
        }));
        if matches!(boundary, Boundary::Pointer) {
            let runtime_id = string(
                &plan["data"]["plan"]["runtime"]["runtime_id"],
                "committed runtime id",
            )?;
            let committed = run_active_engine(
                config,
                case,
                source,
                id,
                &target.identity,
                &target.identity,
                6,
            )?;
            assert_ready(&committed);
            assert_eq!(
                string(
                    &invoke(
                        config,
                        &["server", "instance", "launch-plan", id],
                        "check resolved launch"
                    )?["data"]["native_runtime"]["runtime_id"],
                    "resolved runtime id"
                )?,
                runtime_id
            );
            activate_predecessor(config, id, predecessor, selection, 4)?;
            assert_eq!(
                snapshot_selected_payload(predecessor_engine)?,
                *predecessor_inventory
            );
            let rollback = run_active_engine(
                config,
                case,
                source,
                id,
                &predecessor.identity,
                &predecessor.identity,
                7,
            )?;
            assert_ready(&rollback);
            assert!(!rollback.profile_readback && !rollback.mission_readback);
            let rollback_restart = run_active_engine(
                config,
                case,
                source,
                id,
                &predecessor.identity,
                &predecessor.identity,
                8,
            )?;
            assert!(rollback_restart.profile_readback && rollback_restart.mission_readback);
        }
    }
    assert_eq!(snapshot_selected_payload(source)?, source_inventory);
    Ok(())
}

fn activate_predecessor(
    config: &Path,
    id: &str,
    predecessor: &Prepared,
    selection: u64,
    activation: u64,
) -> io::Result<()> {
    let selected = invoke(
        config,
        &[
            "server",
            "deploy",
            "select-plan",
            id,
            &predecessor.record,
            "--expected-deployment-generation",
            &selection.to_string(),
        ],
        "review predecessor after interruption",
    )?;
    invoke(
        config,
        &[
            "server",
            "deploy",
            "select",
            id,
            &predecessor.record,
            "--expected-deployment-generation",
            &selection.to_string(),
            "--expected-plan-fingerprint",
            string(
                &selected["data"]["plan"]["fingerprint"],
                "predecessor selection fingerprint",
            )?,
            "--yes",
        ],
        "select predecessor after interruption",
    )?;
    let plan = invoke(
        config,
        &[
            "server",
            "deploy",
            "activation",
            "plan",
            id,
            &predecessor.preparation,
            "--expected-deployment-generation",
            &(selection + 1).to_string(),
            "--expected-activation-generation",
            &activation.to_string(),
        ],
        "plan predecessor after interruption",
    )?;
    invoke(
        config,
        &[
            "--yes",
            "server",
            "deploy",
            "activation",
            "apply",
            id,
            &predecessor.preparation,
            "--expected-deployment-generation",
            &(selection + 1).to_string(),
            "--expected-activation-generation",
            &activation.to_string(),
            "--expected-plan-fingerprint",
            string(
                &plan["data"]["plan"]["fingerprint"],
                "predecessor activation fingerprint",
            )?,
        ],
        "activate predecessor after interruption",
    )?;
    Ok(())
}

fn wait_boundary(
    owner: &mut ParentOwner,
    controller: &mut Controller,
    locks: &InstallationLocks,
    activation: &Path,
    boundary: Boundary,
    timeout: Duration,
) -> io::Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
        let maintenance = locks.inspect_maintenance()?;
        let journal = activation.join("journal.json").is_file();
        let generation = read_active_generation(activation)?;
        if boundary.matches(maintenance.pending.is_some(), journal, generation) {
            return Ok(());
        }
        if !controller.is_running()? {
            return Err(io::Error::other(
                "activation controller exited before boundary observation",
            ));
        }
        if !owner.is_running()? {
            return Err(io::Error::other(
                "metadata owner exited before boundary observation",
            ));
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "observation deadline elapsed",
            ));
        }
        thread::sleep(Duration::from_millis(40));
    }
}

fn activation_root(config: &Path, id: &str) -> PathBuf {
    config
        .join("games")
        .join("arma3")
        .join("server")
        .join("instances")
        .join(id)
        .join("deployments")
        .join("activation")
}

fn active_generation(config: &Path, id: &str) -> io::Result<u64> {
    read_active_generation(&activation_root(config, id))
}

fn activation_status(config: &Path, id: &str) -> io::Result<Value> {
    invoke(
        config,
        &["server", "deploy", "activation", "status", id],
        "inspect recovered activation",
    )
}

fn read_active_generation(root: &Path) -> io::Result<u64> {
    let path = root.join("active.json");
    let metadata = fs::symlink_metadata(&path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() || metadata.len() > 64 * 1024 {
        return Err(io::Error::other("Invalid active deployment pointer"));
    }
    let mut bytes = Vec::new();
    fs::File::open(path)?
        .take(64 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 64 * 1024 {
        return Err(io::Error::other("Active pointer exceeds bound"));
    }
    let value: Value = serde_json::from_slice(&bytes)?;
    value["generation"]
        .as_u64()
        .ok_or_else(|| io::Error::other("Active pointer generation missing"))
}

fn directory_names(root: &Path) -> io::Result<Vec<(PathBuf, bool)>> {
    let mut result = Vec::new();
    if !root.exists() {
        return Ok(result);
    }
    let mut stack = vec![(root.to_path_buf(), PathBuf::new(), 0usize)];
    while let Some((directory, relative, depth)) = stack.pop() {
        if depth > 16 || result.len() >= 100_000 {
            return Err(io::Error::other("Runtime path inventory exceeded bounds"));
        }
        for entry in fs::read_dir(directory)? {
            let entry = entry?;
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path)?;
            if metadata.file_type().is_symlink() {
                return Err(io::Error::other("Runtime path inventory found a link"));
            }
            let child = relative.join(entry.file_name());
            let is_dir = metadata.is_dir();
            result.push((child.clone(), is_dir));
            if is_dir {
                stack.push((path, child, depth + 1));
            }
        }
    }
    result.sort();
    Ok(result)
}

fn wait_owner_ready(config: &Path) -> io::Result<()> {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let output = cli(config, &["server", "owner"]).output()?;
        if output.stdout.len() > 1024 * 1024 || output.stderr.len() > 64 * 1024 {
            return Err(io::Error::other("Owner probe output exceeded bounds"));
        }
        if output.status.success() {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(io::Error::other(
                "Parent-bound metadata owner did not become ready",
            ));
        }
        thread::sleep(Duration::from_millis(40));
    }
}
