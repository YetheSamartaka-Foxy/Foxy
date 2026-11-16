use super::*;
use crate::core::server::{ServerContext, ServerStore, ServerStoreError};
use std::fs;

fn fixture() -> (tempfile::TempDir, ServerStore, ServerInstanceId) {
    let root = tempfile::tempdir().unwrap();
    fs::write(
        root.path().join("games.json"),
        include_bytes!("../../../../examples/json/appdata/games.json"),
    )
    .unwrap();
    let store =
        ServerStore::new(ServerContext::capture(root.path(), Some("arma3")).unwrap()).unwrap();
    fs::create_dir_all(store.path().parent().unwrap()).unwrap();
    fs::write(
        store.path(),
        include_bytes!("../../../../examples/json/appdata/games/arma3/server/servers.json"),
    )
    .unwrap();
    let id = store.load().unwrap().instances[0].id.clone();
    (root, store, id)
}

fn input(path: PathBuf, ownership: ConfigurationOwnership) -> ConfigurationImportInput {
    ConfigurationImportInput {
        path,
        ownership,
        role: ConfigurationRole::Server,
    }
}

#[test]
fn operator_templates_preserve_unknown_bytes_and_extract_secret_values_before_review() {
    let (root, store, id) = fixture();
    let path = root.path().join("operator.cfg");
    let source = "// retain this exact comment\r\nhostname = \"Quoted \"\"host\"\"\";\r\nPASSWORDADMIN = \"FixtureOnlyPass987\";\r\nclass Unknown { path = \"R:\\Mods\\Custom Folder\"; items[] = {{1,2},true}; };\r\n";
    fs::write(&path, source).unwrap();
    let mut plan = store
        .plan_configuration_import(
            &id,
            1,
            &[input(path.clone(), ConfigurationOwnership::Operator)],
        )
        .unwrap();
    assert_eq!(plan.report.secret_references.len(), 1);
    let safe = serde_json::to_string(&plan.report).unwrap();
    assert!(!safe.contains("FixtureOnlyPass987"));
    assert_eq!(plan.report.unresolved_paths, [r"R:\Mods\Custom Folder"]);
    assert!(
        !plan
            .report
            .instance
            .configuration_import
            .as_ref()
            .unwrap()
            .paths_reviewed
    );
    plan.set_paths_reviewed(true);
    assert!(
        plan.report
            .instance
            .configuration_import
            .as_ref()
            .unwrap()
            .paths_reviewed
    );
    let template = &plan
        .report
        .instance
        .configuration_import
        .as_ref()
        .unwrap()
        .files[0]
        .template;
    assert!(
        template
            .starts_with("// retain this exact comment\r\nhostname = \"Quoted \"\"host\"\"\";\r\n")
    );
    assert!(template.ends_with(
        "class Unknown { path = \"R:\\Mods\\Custom Folder\"; items[] = {{1,2},true}; };\r\n"
    ));
    assert!(template.contains("PASSWORDADMIN = \"foxy-secret:secret_"));
    assert_eq!(fs::read_to_string(path).unwrap(), source);
    assert!(
        !store
            .context()
            .space_dir()
            .join("server/credentials")
            .exists()
    );
}

#[test]
fn short_secrets_are_not_rejected_because_generated_reference_text_contains_them() {
    for secret in ["x", "secret", ""] {
        let (root, store, id) = fixture();
        let path = root.path().join("operator.cfg");
        fs::write(&path, format!("password = \"{secret}\";\n")).unwrap();
        let plan = store
            .plan_configuration_import(&id, 1, &[input(path, ConfigurationOwnership::Operator)])
            .unwrap();
        assert_eq!(plan.report.secret_references.len(), 1);
    }
}

#[test]
fn unsafe_preprocessing_plaintext_in_templates_and_mask_replacement_fail_without_writes() {
    for text in [
        "#include \"uninspected.cfg\"\npassword=\"hidden\";",
        "password = MACRO;",
        "password = \"<redacted secret reference>\";",
        "password = \"FixturePass\"; // FixturePass",
    ] {
        let (root, store, id) = fixture();
        let path = root.path().join("operator.cfg");
        fs::write(&path, text).unwrap();
        assert!(
            store
                .plan_configuration_import(
                    &id,
                    1,
                    &[input(path.clone(), ConfigurationOwnership::Operator)]
                )
                .is_err()
        );
        assert_eq!(fs::read_to_string(path).unwrap(), text);
        assert!(
            !store
                .context()
                .space_dir()
                .join("server/credentials")
                .exists()
        );
    }
    let unsafe_template = ImportedConfiguration {
        files: vec![ImportedConfigurationFile {
            role: ConfigurationRole::Server,
            ownership: ConfigurationOwnership::Operator,
            template: "password=\"unprotected\";".into(),
        }],
        ..Default::default()
    };
    assert!(unsafe_template.validate().is_err());
}

#[test]
fn pack_import_rederives_paths_and_reports_managed_credentials_without_binding_an_installation() {
    let (_root, store, id) = fixture();
    let mut pack = store.export_instance_pack(&id).unwrap();
    if let crate::core::server::ServerDefinition::Arma3(arma) = &mut pack.definition {
        arma.server.password_ref = Some("secret_00000000000000000000000000000001".into());
    }
    pack.configuration_import = Some(Box::new(ImportedConfiguration {
        files: vec![ImportedConfigurationFile {
            role: ConfigurationRole::Server,
            ownership: ConfigurationOwnership::Operator,
            template: "class Unknown { path=\"R:/Mods/Other Folder\"; };".into(),
        }],
        unresolved_paths: vec![],
        paths_reviewed: true,
        ..Default::default()
    }));
    let mut plan = store
        .plan_instance_pack_import(pack, "ImportedServer")
        .unwrap();
    assert_ne!(plan.report.instance.id, id);
    assert!(plan.report.instance.installation_id.is_none());
    assert_eq!(plan.report.instance.revision, 1);
    assert_eq!(
        plan.report.secret_references,
        ["secret_00000000000000000000000000000001"]
    );
    assert_eq!(plan.report.unresolved_paths, ["R:/Mods/Other Folder"]);
    assert!(
        !plan
            .report
            .instance
            .configuration_import
            .as_ref()
            .unwrap()
            .paths_reviewed
    );
    let old = plan.report.fingerprint.clone();
    plan.set_paths_reviewed(true);
    assert_ne!(old, plan.report.fingerprint);
}

#[test]
fn same_length_import_changes_and_source_edits_invalidate_reviewed_content() {
    let (root, store, id) = fixture();
    let path = root.path().join("operator.cfg");
    let first =
        "class Missions { class One { template=\"CaseOne.World\"; difficulty=\"Custom\"; }; };";
    let second = first.replace("CaseOne", "CaseTwo");
    assert_eq!(first.len(), second.len());
    fs::write(&path, first).unwrap();
    let plan = store
        .plan_configuration_import(
            &id,
            1,
            &[input(path.clone(), ConfigurationOwnership::Managed)],
        )
        .unwrap();
    let old = plan.report.fingerprint.clone();
    fs::write(&path, second).unwrap();
    let updated = store
        .plan_configuration_import(&id, 1, &[input(path, ConfigurationOwnership::Managed)])
        .unwrap();
    assert_ne!(old, updated.report.fingerprint);
    assert!(
        updated
            .report
            .diff
            .iter()
            .any(|line| line.contains("CaseTwo.World"))
    );
    assert!(matches!(
        store.apply_configuration_import(plan),
        Err(ServerStoreError::StaleRevision)
    ));
}

#[test]
fn unresolved_path_limits_fail_instead_of_approving_a_truncated_list() {
    let (root, store, id) = fixture();
    let source = (0..1025)
        .map(|i| format!("path{i}=\"R:/Mods/Folder{i}\";\n"))
        .collect::<String>();
    let path = root.path().join("paths.cfg");
    fs::write(&path, source).unwrap();
    assert!(
        store
            .plan_configuration_import(&id, 1, &[input(path, ConfigurationOwnership::Operator)])
            .is_err()
    );
}

#[test]
fn portable_source_references_keep_same_url_different_path_and_role_order() {
    let (_root, store, id) = fixture();
    let mut pack = store.export_instance_pack(&id).unwrap();
    let sources = ["RepoAlpha", "RepoBeta"].into_iter().enumerate().map(|(index, name)| {
        serde_json::from_value(serde_json::json!({
            "binding": { "source_id": name, "role": if index == 0 { "required" } else { "server_only" }, "order": index },
            "profile": { "name": name, "remote_url": "https://repo.example.invalid:8080/main/", "local_path": format!("R:/Mods/{name}"), "folder_id": name, "supply": "already_present", "addons": ["@mod_01"] },
            "folder": { "id": name, "path": format!("R:/Mods/{name}") }
        })).unwrap()
    }).collect();
    pack.configuration_import = Some(Box::new(ImportedConfiguration {
        source_references: sources,
        ..Default::default()
    }));
    let plan = store
        .plan_instance_pack_import(pack, "ImportedServer")
        .unwrap();
    let imported = plan.report.instance.configuration_import.as_ref().unwrap();
    assert_eq!(imported.source_references.len(), 2);
    assert_eq!(imported.source_references[0].binding.order, 0);
    assert_eq!(imported.source_references[1].binding.order, 1);
    assert_ne!(
        imported.source_references[0].profile.local_path,
        imported.source_references[1].profile.local_path
    );
    assert_eq!(
        plan.report.unresolved_paths,
        ["R:/Mods/RepoAlpha", "R:/Mods/RepoBeta"]
    );
}

#[test]
fn inherited_and_append_semantics_require_operator_ownership() {
    let (root, store, id) = fixture();
    let path = root.path().join("operator.cfg");
    let source = "allowedLoadFileExtensions[] += {\"sqf\"}; class Extra: Base { enabled=1; };";
    fs::write(&path, source).unwrap();
    assert!(
        store
            .plan_configuration_import(
                &id,
                1,
                &[input(path.clone(), ConfigurationOwnership::Managed)]
            )
            .is_err()
    );
    let plan = store
        .plan_configuration_import(&id, 1, &[input(path, ConfigurationOwnership::Operator)])
        .unwrap();
    assert_eq!(
        plan.report
            .instance
            .configuration_import
            .as_ref()
            .unwrap()
            .files[0]
            .template,
        source
    );
}
