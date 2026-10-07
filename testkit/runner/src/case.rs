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
    fn rejects_legacy_oracle() {
        assert!(
            validate(&json!({"id":"a","kind":"ux","steps":[],"guards":{"oracle_command":"a b"}}))
                .unwrap_err()
                .to_string()
                .contains("argv array")
        );
    }
}
