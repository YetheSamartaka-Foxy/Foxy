use super::*;
use crate::core::server::{Arma3Difficulty, Arma3Mission, ServerDefinitionKind};

fn file(path: &str, bytes: u64, hash: &str) -> StageEntry {
    StageEntry {
        path: path.into(),
        kind: StageEntryKind::File,
        bytes,
        blake3: Some(hash.into()),
    }
}

fn arma(templates: &[&str]) -> ServerDefinition {
    let mut definition = ServerDefinition::draft(ServerDefinitionKind::Arma3, "Fixture");
    let ServerDefinition::Arma3(settings) = &mut definition else {
        unreachable!()
    };
    settings.mission_cycle = templates
        .iter()
        .map(|template| Arma3Mission {
            enabled: true,
            source: None,
            params: Default::default(),
            template: (*template).into(),
            difficulty: Arma3Difficulty::Regular,
        })
        .collect();
    definition
}

#[test]
fn disabled_entries_and_addon_class_identities_do_not_use_filename_guesses() {
    let mut definition = arma(&["Disabled.World", "AddonMission"]);
    let ServerDefinition::Arma3(settings) = &mut definition else {
        unreachable!()
    };
    settings.mission_cycle[0].enabled = false;
    settings.mission_cycle[1].source = Some(crate::core::server::Arma3MissionSource::AddonClass);
    let evidence = inspect(
        &definition,
        &[file("addons/@mod/mpmissions/AddonMission.pbo", 4, "aaaa")],
    );
    assert_eq!(evidence.missions.len(), 1);
    assert_eq!(evidence.missions[0].state, "unverified_addon_class");
    assert!(
        evidence
            .issues
            .iter()
            .any(|issue| issue.code == "addon_mission_class_unverified")
    );
    assert!(
        !evidence
            .issues
            .iter()
            .any(|issue| issue.code == "mission_missing")
    );
}

#[test]
fn mission_transfer_limit_uses_the_explicit_engine_http_path() {
    let mut definition = arma(&["Example.World"]);
    let entries = [file(
        "addons/@mod/mpmissions/Example.World.pbo",
        300 * 1024 * 1024 + 1,
        "aaaa",
    )];
    assert!(
        inspect(&definition, &entries)
            .issues
            .iter()
            .any(|issue| issue.code == "mission_transfer_limit")
    );
    let ServerDefinition::Arma3(settings) = &mut definition else {
        unreachable!()
    };
    settings.server.mission_http_download_base_url =
        Some("https://repo.example.invalid:8080/missions/".into());
    assert!(
        !inspect(&definition, &entries)
            .issues
            .iter()
            .any(|issue| issue.code == "mission_transfer_limit")
    );
    let entries = [file(
        "addons/@mod/mpmissions/Example.World.pbo",
        512 * 1024 * 1024 + 1,
        "aaaa",
    )];
    assert!(
        inspect(&definition, &entries)
            .issues
            .iter()
            .any(|issue| issue.code == "mission_transfer_limit")
    );
}

#[test]
fn groups_keys_case_insensitively_and_flags_only_different_bytes() {
    let definition = arma(&[]);
    let evidence = inspect(
        &definition,
        &[
            file("addons/@mod/keys/Server.bikey", 4, "aaaa"),
            file("addons/@mod/nested/server.BIKEY", 5, "bbbb"),
            file("addons/@other/Server.bikey", 4, "aaaa"),
            file("addons/@mod/keys/Other.bikey", 7, "cccc"),
        ],
    );
    let server = evidence
        .keys
        .iter()
        .find(|group| group.name.eq_ignore_ascii_case("server.bikey"))
        .unwrap();
    let other = evidence
        .keys
        .iter()
        .find(|group| group.name.eq_ignore_ascii_case("other.bikey"))
        .unwrap();
    assert_eq!(server.files.len(), 3);
    assert!(server.flattened_collision);
    assert!(!other.flattened_collision);
    assert_eq!(evidence.key_files, 4);
}

#[test]
fn portable_uppercase_collisions_are_reported_across_addon_roots() {
    let evidence = inspect(
        &arma(&[]),
        &[
            file("addons/@mod_01/keys/\u{03c2}.bikey", 4, "aaaa"),
            file("addons/@mod_02/keys/\u{03c3}.bikey", 4, "bbbb"),
        ],
    );
    assert_eq!(evidence.keys.len(), 1);
    assert!(evidence.keys[0].flattened_collision);
    assert!(
        evidence
            .issues
            .iter()
            .any(|issue| issue.code == "flattened_key_collision")
    );
}

#[test]
fn matches_nested_mission_candidates_and_reports_missing_or_conflicting_bytes() {
    let definition = arma(&["Coop.Altis", "Missing.Tanoa"]);
    let evidence = inspect(
        &definition,
        &[
            file("addons/@mod/mpmissions/Coop.Altis.pbo", 20, "aaaa"),
            file(
                "addons/@mod/other/MpMissions/nested/coop.altis.PBO",
                21,
                "bbbb",
            ),
            file("addons/@mod/random/Elsewhere/Other.pbo", 1, "cccc"),
            file("addons/@mod/mpmissions/readme.txt", 1, "dddd"),
        ],
    );
    let coop = evidence
        .missions
        .iter()
        .find(|mission| mission.template == "Coop.Altis")
        .unwrap();
    let missing = evidence
        .missions
        .iter()
        .find(|mission| mission.template == "Missing.Tanoa")
        .unwrap();
    assert_eq!(coop.files.len(), 2);
    assert!(coop.conflicting_bytes);
    assert!(!coop.missing_required);
    assert!(missing.missing_required);
    assert!(missing.files.is_empty());
    assert_eq!(evidence.mission_files, 2);
    assert!(
        evidence
            .issues
            .iter()
            .any(|issue| issue.code == "mission_missing")
    );
}

#[test]
fn exact_duplicate_mission_bytes_are_not_a_conflict_and_templates_are_deduplicated() {
    let definition = arma(&["Coop.Altis", "Coop.Altis"]);
    let evidence = inspect(
        &definition,
        &[
            file("addons/@one/mpmissions/Coop.Altis.pbo", 20, "same"),
            file("addons/@two/mpmissions/coop.altis.PBO", 20, "same"),
        ],
    );
    assert_eq!(evidence.missions.len(), 1);
    assert!(!evidence.missions[0].conflicting_bytes);
    assert!(!evidence.missions[0].missing_required);
}

#[test]
fn candidate_output_is_bounded_and_reforger_has_no_local_artifact_candidates() {
    let definition = arma(&[]);
    let entries = (0..=MAX_REFERENCES)
        .map(|index| {
            file(
                &format!("addons/@mod/keys/key-{index:04}.bikey"),
                1,
                "a".repeat(64).as_str(),
            )
        })
        .collect::<Vec<_>>();
    let evidence = inspect(&definition, &entries);
    assert_eq!(
        evidence
            .keys
            .iter()
            .map(|group| group.files.len())
            .sum::<usize>(),
        MAX_REFERENCES
    );
    assert!(evidence.truncated);
    assert!(
        evidence
            .issues
            .iter()
            .any(|issue| issue.code == "artifact_diagnostics_truncated")
    );
    let reforger = ServerDefinition::draft(ServerDefinitionKind::Reforger, "Fixture");
    let none = inspect(&reforger, &entries);
    assert!(none.keys.is_empty() && none.missions.is_empty());
    assert_eq!(none.key_files, 0);
    assert!(!none.truncated);
}
