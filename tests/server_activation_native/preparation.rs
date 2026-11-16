use std::fs;
use std::io;
use std::path::Path;

use super::{invoke, path_arg, string};

pub(super) struct Prepared {
    pub record: String,
    pub preparation: String,
    pub identity: String,
}

#[derive(Clone, Copy)]
pub(super) struct PreparationRevision {
    pub deployment: u64,
    pub draft: u64,
    pub configuration: u64,
}

pub(super) fn setup_content(config: &Path, case: &Path, id: &str) -> io::Result<()> {
    let source_root = case.join("content-source");
    fs::create_dir_all(&source_root)?;
    invoke(
        config,
        &[
            "server",
            "content",
            "add-folder",
            "--id",
            "RepoAlpha",
            "--path",
            path_arg(&source_root)?,
        ],
        "register content folder",
    )?;
    let local = case.join("profile");
    invoke(
        config,
        &[
            "server",
            "content",
            "add-profile",
            "--name",
            "RepoAlpha",
            "--folder",
            "RepoAlpha",
            "--url",
            "",
            "--local-path",
            path_arg(&local)?,
            "--supply",
            "already-present",
            "--addon",
            "@mod_01",
        ],
        "register server-only profile",
    )?;
    save_bindings(config, case, id)
}

fn save_bindings(config: &Path, case: &Path, id: &str) -> io::Result<()> {
    let bindings = case.join("bindings.json");
    fs::write(
        &bindings,
        br#"[{"source_id":"RepoAlpha","role":"server_only","order":0}]"#,
    )?;
    let plan = invoke(
        config,
        &[
            "server",
            "content",
            "bindings",
            "plan",
            id,
            "--bindings",
            path_arg(&bindings)?,
        ],
        "plan bindings",
    )?;
    invoke(
        config,
        &[
            "server",
            "content",
            "bindings",
            "save",
            id,
            "--bindings",
            path_arg(&bindings)?,
            "--expected-plan-fingerprint",
            string(&plan["data"]["plan"]["fingerprint"], "binding fingerprint")?,
        ],
        "save bindings",
    )?;
    Ok(())
}

pub(super) fn prepare(
    config: &Path,
    case: &Path,
    id: &str,
    instance: &str,
    hostname: &str,
    revision: PreparationRevision,
) -> io::Result<Prepared> {
    let nonce = hostname.to_owned();
    let stage_root = case.join(format!("stage-{:016x}", rand::random::<u64>()));
    let missions = stage_root.join("missions");
    let content = stage_root.join("content");
    fs::create_dir_all(&missions)?;
    let mission = super::super::mission::write(&missions, &content, instance, &nonce)?;
    let addon = content.join("addons");
    let addon_root = addon
        .parent()
        .ok_or_else(|| io::Error::other("Missing addon root"))?;
    let mission_dir = addon_root.join("MPMissions");
    fs::create_dir(&mission_dir)?;
    fs::copy(
        missions.join(format!("{}.pbo", mission.template)),
        mission_dir.join(format!("{}.pbo", mission.template)),
    )?;
    let content_source = case.join("content-source/@mod_01");
    fs::create_dir_all(
        content_source
            .parent()
            .ok_or_else(|| io::Error::other("Missing content source parent"))?,
    )?;
    if content_source.exists() {
        fs::remove_dir_all(&content_source)?;
    }
    fs::rename(content, &content_source)?;
    save_bindings(config, case, id)?;
    let stage_plan = invoke(
        config,
        &["server", "content", "stage", "plan", id],
        "plan content stage",
    )?;
    let stage_id = string(&stage_plan["data"]["plan"]["stage_id"], "stage id")?;
    invoke(
        config,
        &[
            "server",
            "content",
            "stage",
            "apply",
            id,
            "--expected-plan-fingerprint",
            string(
                &stage_plan["data"]["plan"]["fingerprint"],
                "stage fingerprint",
            )?,
        ],
        "apply content stage",
    )?;
    prepare_stage(config, id, stage_id, revision, mission.identity)
}

pub(super) fn prepare_stage(
    config: &Path,
    id: &str,
    stage_id: &str,
    revision: PreparationRevision,
    identity: String,
) -> io::Result<Prepared> {
    let deployment_generation = revision.deployment.to_string();
    let preparation_plan = invoke(
        config,
        &[
            "server",
            "deploy",
            "prepare",
            id,
            "--stage",
            stage_id,
            "--expected-revision",
            &revision.draft.to_string(),
            "--expected-configuration-generation",
            &revision.configuration.to_string(),
            "--expected-deployment-generation",
            &deployment_generation,
        ],
        "plan deployment preparation",
    )?;
    let plan = &preparation_plan["data"]["plan"];
    let record = string(&plan["record_id"], "record id")?.to_owned();
    let fingerprint = string(&plan["fingerprint"], "preparation fingerprint")?;
    invoke(
        config,
        &[
            "server",
            "deploy",
            "commit",
            id,
            "--stage",
            stage_id,
            "--expected-revision",
            &revision.draft.to_string(),
            "--expected-configuration-generation",
            &revision.configuration.to_string(),
            "--expected-deployment-generation",
            &deployment_generation,
            "--expected-plan-fingerprint",
            fingerprint,
        ],
        "commit deployment record",
    )?;
    let selection_generation = (revision.deployment + 1).to_string();
    let runtime_plan = invoke(
        config,
        &[
            "server",
            "deploy",
            "runtime",
            "plan",
            id,
            "--expected-deployment-generation",
            &selection_generation,
        ],
        "plan runtime preparation",
    )?;
    let runtime_fingerprint = string(
        &runtime_plan["data"]["plan"]["fingerprint"],
        "runtime preparation fingerprint",
    )?;
    let built = invoke(
        config,
        &[
            "server",
            "deploy",
            "runtime",
            "build",
            id,
            "--expected-deployment-generation",
            &selection_generation,
            "--expected-plan-fingerprint",
            runtime_fingerprint,
        ],
        "build runtime preparation",
    )?;
    let preparation =
        string(&built["data"]["report"]["preparation_id"], "preparation id")?.to_owned();
    Ok(Prepared {
        record,
        preparation,
        identity,
    })
}
