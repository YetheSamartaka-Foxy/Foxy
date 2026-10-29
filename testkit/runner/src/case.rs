use anyhow::{Context, Result, bail, ensure};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::path::Path;

pub fn read_json(path: &Path) -> Result<Value> {
    let bytes = std::fs::read(path).with_context(|| format!("Reading {}", path.display()))?;
    Ok(serde_json::from_slice(
        bytes.strip_prefix(&[239, 187, 191]).unwrap_or(&bytes),
    )?)
}

pub fn write_json(path: &Path, value: &Value) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, format!("{}\n", serde_json::to_string_pretty(value)?))?;
    Ok(())
}

pub fn load(path: &Path, repo_root: &Path, run_dir: &Path) -> Result<Value> {
    let mut value = read_json(path)?;
    expand(&mut value, repo_root, run_dir)?;
    validate(&value)?;
    Ok(value)
}

fn expand(value: &mut Value, root: &Path, run: &Path) -> Result<()> {
    match value {
        Value::String(text) => {
            *text = text
                .replace("${REPO_ROOT}", &root.to_string_lossy())
                .replace("${RUN_DIR}", &run.to_string_lossy());
            let pattern = regex::Regex::new(r"\$\{ENV:([A-Za-z_][A-Za-z0-9_]*)\}")?;
            let original = text.clone();
            for capture in pattern.captures_iter(&original) {
                let replacement = std::env::var(&capture[1]).with_context(|| {
                    format!(
                        "Environment variable {} is required by the case",
                        &capture[1]
                    )
                })?;
                *text = text.replace(&capture[0], &replacement);
            }
        }
        Value::Array(items) => {
            for item in items {
                expand(item, root, run)?;
            }
        }
        Value::Object(items) => {
            for item in items.values_mut() {
                expand(item, root, run)?;
            }
        }
        _ => {}
    }
    Ok(())
}

pub fn validate(case: &Value) -> Result<()> {
    ensure!(
        regex::Regex::new("^[a-z0-9]+(?:-[a-z0-9]+)*$")?
            .is_match(case["id"].as_str().unwrap_or("")),
        "Case id must be kebab-case"
    );
    let kind = case["kind"].as_str().unwrap_or("");
    ensure!(
        matches!(kind, "ux" | "perf"),
        "Case kind must be ux or perf"
    );
    let harness = case["harness"].as_str().unwrap_or("gui");
    ensure!(
        matches!(harness, "gui" | "cli"),
        "Case harness must be gui or cli"
    );
    if kind == "ux" {
        ensure!(harness == "gui", "UX cases require the gui harness");
        ensure!(case["steps"].is_array(), "UX case requires steps");
    }
    if kind == "perf" {
        ensure!(
            case["repository"].is_object(),
            "Perf case requires repository"
        );
        ensure!(
            case["operations"].is_array(),
            "Perf case requires operations"
        );
        for operation in case["operations"].as_array().into_iter().flatten() {
            if matches!(
                operation["op"].as_str(),
                Some(
                    "cancel_after_promoted_file"
                        | "kill_after_promoted_file"
                        | "close_after_promoted_file"
                )
            ) {
                bail!(
                    "Use cancel_after_promoted_file or kill_after_promoted_file as a boolean on a download operation"
                );
            }
            if operation["cancel_after_promoted_file"].as_bool() == Some(true)
                || operation["kill_after_promoted_file"].as_bool() == Some(true)
                || operation["close_after_promoted_file"].as_bool() == Some(true)
                || operation["pause_after_s"].as_f64().is_some()
            {
                ensure!(
                    case["harness"].as_str().unwrap_or("gui") == "gui"
                        && matches!(
                            operation["op"].as_str(),
                            Some("download" | "force-redownload")
                        ),
                    "Promotion waits and pause_after_s require GUI download operations"
                );
            }
            if !operation["external_edit_after_promoted_file"].is_null() {
                ensure!(
                    operation["external_edit_after_promoted_file"].is_string(),
                    "external_edit_after_promoted_file must be a string"
                );
                ensure!(
                    operation["cancel_after_promoted_file"].as_bool() == Some(true),
                    "external_edit_after_promoted_file requires cancel_after_promoted_file"
                );
            }
            if !operation["cancel_after_hash_progress"].is_null() {
                let threshold = operation["cancel_after_hash_progress"]
                    .as_f64()
                    .context("cancel_after_hash_progress must be a number between 0 and 1")?;
                ensure!(
                    threshold > 0.0 && threshold < 1.0,
                    "cancel_after_hash_progress must be greater than 0 and less than 1"
                );
                ensure!(
                    harness == "gui" && operation["op"].as_str() == Some("recheck-integrity"),
                    "cancel_after_hash_progress requires a GUI recheck-integrity operation"
                );
            }
            if matches!(
                operation["op"].as_str(),
                Some(
                    "assert-download-paused"
                        | "assert-repository-paused"
                        | "assert-space-bulk-target-count"
                        | "assert-space-bulk-completed-count"
                        | "assert-space-bulk-failed-count"
                        | "wait-for-space-bulk-target-count"
                        | "prepare-test-directory"
                        | "capture-origin-requests"
                        | "assert-origin-requests-unchanged"
                        | "assert-pending-update-count"
                        | "invoke-driver"
                        | "wait-for-busy"
                )
            ) {
                ensure!(harness == "gui", "State operations require the GUI harness");
            }
            if matches!(
                operation["op"].as_str(),
                Some("capture-file-state" | "assert-file-state")
            ) {
                ensure!(
                    operation["label"].is_string(),
                    "File-state operations require a label"
                );
            }
            if operation["op"].as_str() == Some("cli-command") {
                ensure!(harness == "cli", "cli-command requires the cli harness");
                let args = operation["args"]
                    .as_array()
                    .context("cli-command requires an args array")?;
                ensure!(!args.is_empty(), "cli-command args cannot be empty");
                let args = args
                    .iter()
                    .map(|value| value.as_str().context("cli-command args must be strings"))
                    .collect::<Result<Vec<_>>>()?;
                ensure!(
                    matches!(
                        args.as_slice(),
                        ["config", "export", _]
                            | ["config", "import", _, "--yes"]
                            | ["config", "extra-file", "list"]
                            | [
                                "config",
                                "extra-file",
                                "add",
                                "--name",
                                _,
                                "--source",
                                _,
                                "--destination",
                                _
                            ]
                            | ["workshop", "list"]
                            | ["workshop", "add", _, "--backend", "none", "--skip-metadata"]
                            | ["workshop", "set", _, "--enabled", "true" | "false"]
                            | ["workshop", "remove", _, "--delete-data", "--yes"]
                            | ["workshop", "bundle", "export", _, "--no-payloads"]
                            | [
                                "workshop",
                                "bundle",
                                "import",
                                _,
                                "--mode",
                                "merge" | "match",
                                "--skip-payloads",
                                "--backend",
                                "none",
                                "--skip-metadata",
                                "--yes"
                            ]
                            | ["profile", "add", "--repo-name", _, "--profile", _]
                            | ["profile", "select", "--repo-name", _, "--profile", _]
                            | ["addon", "list", "--repo-name", _]
                    ),
                    "cli-command supports only isolated config, profile, addon-query and offline Workshop commands"
                );
                ensure!(
                    operation["expected_exit_code"]
                        .as_u64()
                        .is_some_and(|code| code <= 255),
                    "cli-command requires expected_exit_code between 0 and 255"
                );
                ensure!(
                    operation["path_arguments"].as_array().is_some(),
                    "cli-command requires path_arguments"
                );
                let path_arguments = operation["path_arguments"].as_array().unwrap();
                let expected_path_arguments: &[u64] = match args.as_slice() {
                    ["config", "export", _] | ["config", "import", _, "--yes"] => &[2],
                    ["workshop", "bundle", "export", _, "--no-payloads"]
                    | [
                        "workshop",
                        "bundle",
                        "import",
                        _,
                        "--mode",
                        _,
                        "--skip-payloads",
                        "--backend",
                        "none",
                        "--skip-metadata",
                        "--yes",
                    ] => &[3],
                    [
                        "config",
                        "extra-file",
                        "add",
                        "--name",
                        _,
                        "--source",
                        _,
                        "--destination",
                        _,
                    ] => &[6, 8],
                    ["config", "extra-file", "list"] | ["workshop", "list"] => &[],
                    ["workshop", "add", _, "--backend", "none", "--skip-metadata"]
                    | ["workshop", "set", _, "--enabled", _]
                    | ["workshop", "remove", _, "--delete-data", "--yes"]
                    | ["profile", "add", "--repo-name", _, "--profile", _]
                    | ["profile", "select", "--repo-name", _, "--profile", _]
                    | ["addon", "list", "--repo-name", _] => &[],
                    _ => unreachable!(),
                };
                ensure!(
                    path_arguments
                        .iter()
                        .map(|value| value.as_u64())
                        .collect::<Option<Vec<_>>>()
                        .as_deref()
                        == Some(expected_path_arguments),
                    "cli-command path_arguments must identify each file argument exactly"
                );
                ensure!(
                    operation["expect"].as_array().is_some(),
                    "cli-command requires an expect array"
                );
            }
            if matches!(
                operation["op"].as_str(),
                Some("acquire-test-file-lock" | "release-test-file-lock")
            ) {
                ensure!(
                    operation["path"].is_string(),
                    "Test file lock operations require a run-local path"
                );
                ensure!(
                    operation["allow_read"].is_null() || operation["allow_read"].is_boolean(),
                    "Test file lock allow_read must be boolean"
                );
                ensure!(
                    if harness == "cli" {
                        operation["allow_read"].as_bool() == Some(true)
                    } else {
                        harness == "gui" && operation["allow_read"].as_bool() != Some(true)
                    },
                    "CLI test file locks require allow_read:true; GUI test file locks remain exclusive"
                );
            }
            if operation["op"].as_str() == Some("write-file") {
                ensure!(
                    operation["path"].is_string() && operation["content"].is_string(),
                    "write-file requires a run-local path and string content"
                );
                ensure!(
                    operation["overwrite_existing"].is_null()
                        || operation["overwrite_existing"].is_boolean(),
                    "write-file overwrite_existing must be boolean"
                );
            }
            if operation["op"].as_str() == Some("replace-test-file-with-directory") {
                ensure!(
                    harness == "cli" && operation["path"].is_string(),
                    "replace-test-file-with-directory requires the cli harness and a run-local path"
                );
            }
            if operation["op"].as_str() == Some("move-origin-addon-category") {
                ensure!(
                    harness == "cli"
                        && case["origin"].is_object()
                        && operation["root"].is_string()
                        && operation["addon"].is_string()
                        && matches!(operation["from"].as_str(), Some("required" | "optional"))
                        && matches!(operation["to"].as_str(), Some("required" | "optional"))
                        && operation["from"] != operation["to"],
                    "move-origin-addon-category requires a CLI origin, run-local root, addon, and distinct required/optional categories"
                );
            }
            if matches!(
                operation["op"].as_str(),
                Some("assert-file-exists" | "assert-file-absent")
            ) {
                ensure!(
                    operation["path"].is_string(),
                    "File existence assertions require a run-local path"
                );
            }
            if operation["op"].as_str() == Some("assert-pending-update-count") {
                ensure!(
                    operation["minimum"].as_u64().is_some(),
                    "assert-pending-update-count requires a non-negative minimum"
                );
                ensure!(
                    operation["maximum"].as_u64().is_none_or(|maximum| {
                        maximum >= operation["minimum"].as_u64().unwrap_or(u64::MAX)
                    }),
                    "assert-pending-update-count maximum must be at least minimum"
                );
                ensure!(
                    operation["addons"]
                        .as_array()
                        .is_none_or(|addons| addons.iter().all(Value::is_string)),
                    "assert-pending-update-count addons must be an array of names"
                );
            }
            if matches!(
                operation["op"].as_str(),
                Some("capture-origin-requests" | "assert-origin-requests-unchanged")
            ) {
                ensure!(
                    case["origin"].is_object() && operation["label"].is_string(),
                    "Origin request state operations require a case origin and label"
                );
            }
            let recovery_modes = [
                operation["cancel_after_promoted_file"].as_bool() == Some(true),
                operation["kill_after_promoted_file"].as_bool() == Some(true),
                operation["close_after_promoted_file"].as_bool() == Some(true),
                operation["pause_after_s"].as_f64().is_some(),
                operation["cancel_after_s"].as_f64().is_some(),
                operation["cancel_after_hash_progress"].is_number(),
                operation["kill_after_s"].as_f64().is_some(),
            ];
            ensure!(
                recovery_modes
                    .into_iter()
                    .filter(|enabled| *enabled)
                    .count()
                    <= 1,
                "An operation may select only one cancel, close, kill, or pause trigger"
            );
            if operation["op"].as_str() == Some("origin-switch") {
                ensure!(
                    operation["root"].is_string(),
                    "origin-switch requires a root directory"
                );
                ensure!(
                    case["origin"]["root"].is_string(),
                    "origin-switch requires a case origin"
                );
            }
        }
    }
    if let Some(versions) = case["origin"]["synthetic"].as_array() {
        ensure!(
            !versions.is_empty(),
            "origin.synthetic must contain at least one version"
        );
        for version in versions {
            ensure!(
                version["root"].is_string(),
                "Each synthetic origin version requires a root"
            );
            for field in ["mods", "files_per_mod", "file_bytes", "entry_bytes"] {
                if let Some(value) = version[field].as_u64() {
                    ensure!(
                        field == "mods" || value > 0,
                        "origin.synthetic.{field} must be positive"
                    );
                }
            }
            if let Some(seeds) = version["mod_seeds"].as_array() {
                ensure!(
                    seeds.len() == version["mods"].as_u64().unwrap_or(1) as usize
                        && seeds.iter().all(|seed| seed.as_i64().is_some_and(|seed| {
                            (i64::from(i32::MIN)..=i64::from(i32::MAX)).contains(&seed)
                        })),
                    "origin.synthetic.mod_seeds must contain one 32-bit seed per mod"
                );
            }
        }
    }
    let oracle = &case["guards"]["oracle_command"];
    if !oracle.is_null()
        && !oracle
            .as_array()
            .is_some_and(|argv| !argv.is_empty() && argv.iter().all(Value::is_string))
    {
        bail!(
            "guards.oracle_command must be an argv array, for example [\"foxy-testkit-oracle\", \"--repository-path\", \"${{REPOSITORY_PATH}}\"]; replace the legacy command string"
        );
    }
    Ok(())
}

pub fn hash(case: &Value) -> Result<String> {
    Ok(hex::encode(Sha256::digest(serde_jcs::to_vec(case)?)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn canonical_hash_ignores_object_order() {
        assert_eq!(
            hash(&json!({"b":2,"a":1})).unwrap(),
            hash(&json!({"a":1,"b":2})).unwrap()
        );
        assert_ne!(hash(&json!([1, 2])).unwrap(), hash(&json!([2, 1])).unwrap());
    }
    #[test]
    fn origin_switch_requires_a_root_and_a_case_origin() {
        let case = |origin: Value, switch: Value| json!({"id":"a","kind":"perf","repository":{},"origin":origin,"operations":[switch]});
        let switch = json!({"op":"origin-switch","root":"v2"});
        assert!(validate(&case(json!({"root":"v1","port":8140}), switch.clone())).is_ok());
        assert!(validate(&case(Value::Null, switch)).is_err());
        assert!(validate(&case(json!({"root":"v1"}), json!({"op":"origin-switch"}))).is_err());
    }

    #[test]
    fn per_mod_synthetic_seeds_match_mod_count_and_i32_range() {
        let case = |seeds: Value| {
            json!({
                "id":"synthetic-seeds",
                "kind":"perf",
                "repository":{},
                "origin":{"root":"v1","synthetic":[{"root":"v1","mods":2,"mod_seeds":seeds}]},
                "operations":[]
            })
        };
        assert!(validate(&case(json!([11, 22]))).is_ok());
        assert!(validate(&case(json!([11]))).is_err());
        assert!(validate(&case(json!([11, i64::from(i32::MAX) + 1]))).is_err());
    }

    #[test]
    fn synthetic_origin_can_publish_zero_mods_but_not_zero_file_dimensions() {
        let case = |mods, files_per_mod| {
            json!({
                "id":"empty-publication",
                "kind":"perf",
                "repository":{},
                "origin":{"root":"v1","synthetic":[{
                    "root":"v1",
                    "mods":mods,
                    "files_per_mod":files_per_mod,
                    "file_bytes":1,
                    "entry_bytes":1
                }]},
                "operations":[]
            })
        };
        assert!(validate(&case(0, 1)).is_ok());
        assert!(validate(&case(1, 0)).is_err());
    }

    #[test]
    fn hash_progress_cancellation_requires_an_integrity_operation_and_fraction() {
        let case = |operation| {
            json!({
                "id":"integrity-cancel",
                "kind":"perf",
                "harness":"gui",
                "repository":{},
                "operations":[operation]
            })
        };
        assert!(
            validate(&case(json!({
                "op":"recheck-integrity",
                "cancel_after_hash_progress":0.4
            })))
            .is_ok()
        );
        assert!(
            validate(&case(json!({
                "op":"recheck-integrity",
                "cancel_after_hash_progress":0.0
            })))
            .is_err()
        );
        assert!(
            validate(&case(json!({
                "op":"recheck-integrity",
                "cancel_after_hash_progress":1.0
            })))
            .is_err()
        );
        assert!(
            validate(&case(json!({
                "op":"download",
                "cancel_after_hash_progress":0.4
            })))
            .is_err()
        );
        assert!(
            validate(&case(json!({
                "op":"recheck-integrity",
                "cancel_after_hash_progress":0.4,
                "cancel_after_s":0.1
            })))
            .is_err()
        );
    }

    #[test]
    fn test_file_lock_harness_requires_explicit_read_permission_for_cli() {
        let case = |harness, operation| {
            json!({
                "id":"file-lock",
                "kind":"perf",
                "harness":harness,
                "repository":{},
                "operations":[operation]
            })
        };
        assert!(
            validate(&case(
                "gui",
                json!({"op":"acquire-test-file-lock","path":"${RUN_DIR}/repo/file.bin"})
            ))
            .is_ok()
        );
        assert!(validate(&case("gui", json!({"op":"release-test-file-lock"}))).is_err());
        assert!(
            validate(&case(
                "cli",
                json!({"op":"acquire-test-file-lock","path":"${RUN_DIR}/repo/file.bin"})
            ))
            .is_err()
        );
        assert!(validate(&case(
            "gui",
            json!({"op":"acquire-test-file-lock","path":"${RUN_DIR}/repo/file.bin","allow_read":true})
        )).is_err());
        assert!(validate(&case(
            "cli",
            json!({"op":"acquire-test-file-lock","path":"${RUN_DIR}/config/games/arma3/workshop.json","allow_read":false})
        )).is_err());
        assert!(validate(&case(
            "cli",
            json!({"op":"acquire-test-file-lock","path":"${RUN_DIR}/config/games/arma3/workshop.json","allow_read":true})
        )).is_ok());
        assert!(validate(&case(
            "cli",
            json!({"op":"release-test-file-lock","path":"${RUN_DIR}/config/games/arma3/workshop.json","allow_read":true})
        )).is_ok());
    }

    #[test]
    fn pending_update_count_bounds_are_validated() {
        let case = |maximum| {
            json!({
                "id":"pending-count",
                "kind":"perf",
                "repository":{},
                "operations":[{"op":"assert-pending-update-count","minimum":1,"maximum":maximum}]
            })
        };
        assert!(validate(&case(1)).is_ok());
        assert!(validate(&case(0)).is_err());
    }

    #[test]
    fn rejects_legacy_oracle() {
        assert!(
            validate(&json!({"id":"a","kind":"ux","steps":[],"guards":{"oracle_command":"a b"}}))
                .unwrap_err()
                .to_string()
                .contains("argv array")
        );
    }

    #[test]
    fn accepts_isolated_config_pack_cli_scenarios() {
        let case = json!({
            "id":"state-n05-pack",
            "kind":"perf",
            "harness":"cli",
            "repository":{"name":"RepoAlpha","address":"http://127.0.0.1:8154/","path":"R:/run/repo"},
            "operations":[{
                "op":"cli-command",
                "args":["config","import","R:/run/pack.foxypack","--yes"],
                "path_arguments":[2],
                "expected_exit_code":4,
                "expect":[
                    {"path":"ok","equals":false},
                    {"path":"action","equals":"config.import"},
                    {"path":"data","equals":null}
                ]
            }]
        });
        assert!(validate(&case).is_ok());
    }

    #[test]
    fn offline_workshop_scenarios_cannot_enable_a_network_backend_or_hide_bundle_paths() {
        let case = |args: Value, paths: Value| {
            json!({
                "id":"state-workshop", "kind":"perf", "harness":"cli", "repository":{},
                "operations":[{"op":"cli-command", "args":args, "path_arguments":paths,
                    "expected_exit_code":0, "expect":[]}]
            })
        };
        let args = json!([
            "workshop",
            "bundle",
            "import",
            "R:/run/setup.foxyshare",
            "--mode",
            "match",
            "--skip-payloads",
            "--backend",
            "none",
            "--skip-metadata",
            "--yes"
        ]);
        assert!(validate(&case(args.clone(), json!([3]))).is_ok());
        assert!(validate(&case(args.clone(), json!([]))).is_err());
        let mut network = args;
        network[8] = json!("steam-helper");
        assert!(validate(&case(network, json!([3]))).is_err());
        assert!(
            validate(&case(
                json!(["workshop", "unsubscribe", "123456789", "--yes"]),
                json!([])
            ))
            .is_err()
        );
    }

    #[test]
    fn rejects_cli_scenario_paths_not_declared_as_run_local_arguments() {
        let case = json!({
            "id":"state-n05-pack",
            "kind":"perf",
            "harness":"cli",
            "repository":{"name":"RepoAlpha","address":"http://127.0.0.1:8154/","path":"R:/run/repo"},
            "operations":[{
                "op":"cli-command",
                "args":["config","export","C:/outside.foxypack"],
                "path_arguments":[],
                "expected_exit_code":0,
                "expect":[]
            }]
        });
        assert!(validate(&case).is_err());
    }

    #[test]
    fn validates_category_reclassification_scenarios_and_cli_profile_queries() {
        let case = json!({
            "id":"state-n16-category-reclassification",
            "kind":"perf",
            "harness":"cli",
            "origin":{"root":"R:/run/origin"},
            "repository":{"name":"RepoAlpha","address":"http://127.0.0.1:8154/","path":"R:/run/repo"},
            "operations":[
                {"op":"move-origin-addon-category","root":"R:/run/origin","addon":"@synthetic_000","from":"required","to":"optional"},
                {"op":"cli-command","args":["addon","list","--repo-name","RepoAlpha"],"path_arguments":[],"expected_exit_code":0,"expect":[]}
            ]
        });
        assert!(validate(&case).is_ok());
        let invalid = json!({
            "id":"state-n16-category-reclassification",
            "kind":"perf",
            "harness":"cli",
            "origin":{"root":"R:/run/origin"},
            "repository":{"name":"RepoAlpha","address":"http://127.0.0.1:8154/","path":"R:/run/repo"},
            "operations":[{"op":"move-origin-addon-category","root":"C:/outside","addon":"@synthetic_000","from":"required","to":"required"}]
        });
        assert!(validate(&invalid).is_err());
    }

    #[test]
    fn external_edit_injection_requires_promoted_file_cancellation() {
        let case = json!({
            "id":"state-n01-external-edit-rollback",
            "kind":"perf",
            "harness":"gui",
            "repository":{"name":"RepoAlpha","address":"http://127.0.0.1:8154/","path":"R:/run/repo"},
            "operations":[{
                "op":"download",
                "cancel_after_promoted_file":true,
                "external_edit_after_promoted_file":"external edit"
            }]
        });
        assert!(validate(&case).is_ok());

        let mut invalid = case.clone();
        invalid["operations"][0]["cancel_after_promoted_file"] = json!(false);
        assert!(validate(&invalid).is_err());

        let mut invalid = case;
        invalid["operations"][0]["external_edit_after_promoted_file"] = json!(true);
        assert!(validate(&invalid).is_err());
    }
}
