use std::{
    collections::{HashMap, HashSet},
    process::Command,
};

// cargo-deny skips path dependencies in its bans check, so it cannot enforce
// the oracle's independence from workspace and other local implementations.
#[test]
fn oracle_depends_on_nothing_in_this_workspace() {
    let output = Command::new(env!("CARGO"))
        .args(["metadata", "--format-version", "1", "--locked"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("run cargo metadata");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let metadata: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let packages: HashMap<&str, &serde_json::Value> = metadata["packages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|package| (package["id"].as_str().unwrap(), package))
        .collect();
    let oracle = packages
        .values()
        .find(|package| package["name"] == "foxy-testkit-oracle")
        .expect("oracle workspace member");
    let root = oracle["id"].as_str().unwrap();
    let nodes: HashMap<&str, &serde_json::Value> = metadata["resolve"]["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|node| (node["id"].as_str().unwrap(), node))
        .collect();
    let mut seen = HashSet::from([root]);
    let mut pending = vec![root];
    while let Some(id) = pending.pop() {
        for dependency in nodes[id]["dependencies"].as_array().unwrap() {
            let dependency = dependency.as_str().unwrap();
            assert!(
                !packages[dependency]["source"].is_null(),
                "oracle depends on local package: {}",
                packages[dependency]["name"]
            );
            if seen.insert(dependency) {
                pending.push(dependency);
            }
        }
    }
}
