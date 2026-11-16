use super::{is_scenario_resource, official_scenario_lines};

#[test]
fn accepts_only_counted_official_scenario_listing_rows() {
    let output = concat!(
        "SCRIPT : GetResourceObject \"{0011223344556677}Missions/Noise.conf\"\n",
        "08:43:25.153 SCRIPT       : Official scenarios (2 entries)\n",
        "08:43:25.153 SCRIPT       : --------------------------------------------------\n",
        "08:43:25.153 SCRIPT       : {0011223344556677}Missions/Conflict.conf (Conflict)\n",
        "08:43:25.153 SCRIPT       : {8899AABBCCDDEEFF}Missions/Tutorial.conf (Training)\n",
        "SCRIPT : unrelated trailing message\n",
    );
    let rows = official_scenario_lines(output).unwrap();
    assert_eq!(rows.len(), 2);
    assert!(rows[0].contains("Missions/Conflict.conf"));
    assert!(rows[1].contains("Missions/Tutorial.conf"));
}

#[test]
fn rejects_unscoped_resources_malformed_guids_and_count_mismatches() {
    assert!(official_scenario_lines("SCRIPT : {0011223344556677}Missions/Noise.conf\n").is_none());
    for invalid in [
        "{001122334455667}Missions/Bad.conf",
        "{0011223344556677}Other/Bad.conf",
        "{0011223344556677}Missions/../Bad.conf",
    ] {
        assert!(!is_scenario_resource(invalid));
    }
    assert!(
        official_scenario_lines(concat!(
            "SCRIPT : Official scenarios (2 entries)\n",
            "SCRIPT : {0011223344556677}Missions/OnlyOne.conf (Only one)\n",
        ))
        .is_none()
    );
}
