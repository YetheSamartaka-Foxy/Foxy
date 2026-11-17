use super::*;

const GUID: &str = "0123456789abcdef0123456789abcdef";
const VERIFIED_PLAYERS: &str = "Players on server:\n[#] [IP Address] [Ping] [GUID] [Name]\n--------------------------------------------------\n0 127.0.0.1:2306 -1 0123456789abcdef0123456789abcdef(OK) headless client\n(1 players in total)";
const EMPTY_BANS: &str = "GUID Bans:\n[#] [GUID] [Minutes left] [Reason]\n----------------------------------------\n\nIP Bans:\n[#] [IP Address] [Minutes left] [Reason]\n----------------------------------------------";
const POPULATED_BANS: &str = "GUID Bans:\n[#] [GUID] [Minutes left] [Reason]\n----------------------------------------\n0  0123456789abcdef0123456789abcdef 0    test reason\n\nIP Bans:\n[#] [IP Address] [Minutes left] [Reason]\n----------------------------------------------\n1  127.0.0.1:2306 0    test address reason";
const EMPTY_MISSIONS: &str = "Missions on server:";

#[test]
fn command_wire_text_and_required_preflight_match_native_verbs() {
    let commands = [
        (ArmaAdminCommand::Players, "players", None),
        (ArmaAdminCommand::Bans, "bans", None),
        (ArmaAdminCommand::Missions, "missions", None),
        (
            ArmaAdminCommand::Announce {
                message: "Server restart soon".into(),
            },
            "say -1 Server restart soon",
            None,
        ),
        (
            ArmaAdminCommand::Kick {
                player_id: 7,
                expected_guid: GUID.into(),
                reason: "Policy violation".into(),
            },
            "kick 7 Policy violation",
            Some("players"),
        ),
        (
            ArmaAdminCommand::AddBan {
                guid: GUID.into(),
                minutes: 60,
                reason: "Policy violation".into(),
            },
            "addBan 0123456789abcdef0123456789abcdef 60 Policy violation",
            None,
        ),
        (
            ArmaAdminCommand::RemoveBan {
                ban_id: 4,
                expected_guid: GUID.into(),
            },
            "removeBan 4",
            Some("bans"),
        ),
        (ArmaAdminCommand::Lock, "#lock", None),
        (ArmaAdminCommand::Unlock, "#unlock", None),
        (
            ArmaAdminCommand::ChangeMission {
                template: "mission_alpha.vr".into(),
            },
            "#mission mission_alpha.vr",
            Some("missions"),
        ),
        (ArmaAdminCommand::RestartMission, "#restart", None),
    ];
    for (command, expected, preflight) in commands {
        assert_eq!(command.wire_text().unwrap(), expected);
        assert_eq!(command.required_preflight(), preflight);
        let encoded = serde_json::to_string(&command).unwrap();
        assert_eq!(
            serde_json::from_str::<ArmaAdminCommand>(&encoded).unwrap(),
            command
        );
    }
}

#[test]
fn command_json_is_strict_and_roundtrips() {
    let command = ArmaAdminCommand::Kick {
        player_id: 3,
        expected_guid: GUID.into(),
        reason: "rule breach".into(),
    };
    let json = serde_json::to_string(&command).unwrap();
    assert_eq!(
        serde_json::from_str::<ArmaAdminCommand>(&json).unwrap(),
        command
    );
    assert!(serde_json::from_str::<ArmaAdminCommand>(
        r#"{"kind":"kick","player_id":3,"expected_guid":"0123456789abcdef0123456789abcdef","reason":"x","extra":true}"#
    )
    .is_err());
    assert!(serde_json::from_str::<ArmaAdminCommand>(r#"{"kind":"shutdown"}"#).is_err());
}

#[test]
fn command_rejects_line_injection_and_invalid_operands() {
    for command in [
        ArmaAdminCommand::Announce {
            message: "hello\n#shutdown".into(),
        },
        ArmaAdminCommand::Kick {
            player_id: 1,
            expected_guid: GUID.into(),
            reason: "x\rmore".into(),
        },
        ArmaAdminCommand::AddBan {
            guid: "not-a-guid".into(),
            minutes: 10,
            reason: "valid".into(),
        },
        ArmaAdminCommand::RemoveBan {
            ban_id: 1,
            expected_guid: "0".into(),
        },
    ] {
        assert!(command.wire_text().is_err());
    }
    assert!(
        ArmaAdminCommand::ChangeMission {
            template: "../mission.Altis".into()
        }
        .wire_text()
        .is_err()
    );
    assert!(
        ArmaAdminCommand::ChangeMission {
            template: "mission.Altis.pbo".into()
        }
        .wire_text()
        .is_err()
    );
    assert!(
        ArmaAdminCommand::ChangeMission {
            template: "mission;#shutdown.Altis".into()
        }
        .wire_text()
        .is_err()
    );
    assert!(
        ArmaAdminCommand::Announce {
            message: "\u{00e9}".repeat(129)
        }
        .wire_text()
        .is_err()
    );
}

#[test]
fn player_parser_requires_complete_counted_roster_and_preserves_utf8_names() {
    let parsed = parse_players(VERIFIED_PLAYERS);
    assert!(parsed.complete);
    assert_eq!(parsed.players.len(), 1);
    assert_eq!(parsed.players[0].player_id, 0);
    assert_eq!(parsed.players[0].ping, -1);
    assert_eq!(parsed.players[0].guid.as_deref(), Some(GUID));
    assert!(parsed.players[0].verified);
    assert_eq!(parsed.players[0].name, "headless client");

    let utf8 = VERIFIED_PLAYERS.replace("headless client", "J\u{00f6}rg \u{73a9}\u{5bb6}");
    assert_eq!(
        parse_players(&utf8).players[0].name,
        "J\u{00f6}rg \u{73a9}\u{5bb6}"
    );
    let empty = "Players on server:\n[#] [IP Address] [Ping] [GUID] [Name]\n------------------------\n(0 players in total)";
    assert!(parse_players(empty).complete);

    for invalid in [
        VERIFIED_PLAYERS.replace("(1 players in total)", "(2 players in total)"),
        VERIFIED_PLAYERS.replace("(1 players in total)", ""),
        VERIFIED_PLAYERS.replace("0 127.0.0.1:2306", "bad 127.0.0.1:2306"),
        VERIFIED_PLAYERS.replace("(1 players in total)", "(1 players in total)\ntrailing"),
    ] {
        assert!(!parse_players(&invalid).complete);
    }
    let duplicate = VERIFIED_PLAYERS.replace(
        "(1 players in total)",
        &format!("0 127.0.0.1:2307 20 {GUID}(OK) duplicate\n(2 players in total)"),
    );
    assert!(!parse_players(&duplicate).complete);
    assert!(!parse_players(&VERIFIED_PLAYERS[..VERIFIED_PLAYERS.len() - 8]).complete);
}

#[test]
fn kick_preflight_requires_fresh_complete_roster_and_matching_verified_guid() {
    let command = ArmaAdminCommand::Kick {
        player_id: 0,
        expected_guid: GUID.into(),
        reason: "test".into(),
    };
    assert!(command.validate_preflight(VERIFIED_PLAYERS).is_ok());
    assert!(
        command
            .validate_preflight(&VERIFIED_PLAYERS.replace(GUID, "abcdefabcdefabcdefabcdefabcdefab"))
            .is_err()
    );
    assert!(
        command
            .validate_preflight(&VERIFIED_PLAYERS.replace("(OK)", ""))
            .is_err()
    );
    assert!(
        command
            .validate_preflight(&VERIFIED_PLAYERS.replace("(1 players in total)", ""))
            .is_err()
    );
}

#[test]
fn ban_parser_accepts_both_empty_sections_and_rejects_truncation_or_duplicate_ids() {
    let empty = parse_bans(EMPTY_BANS);
    assert!(empty.complete);
    assert!(empty.bans.is_empty());

    let parsed = parse_bans(POPULATED_BANS);
    assert!(parsed.complete);
    assert_eq!(parsed.bans.len(), 2);
    assert_eq!(parsed.bans[0].guid.as_deref(), Some(GUID));
    assert_eq!(parsed.bans[0].minutes_left, "0");
    assert_eq!(parsed.bans[0].reason, "test reason");
    assert_eq!(parsed.bans[1].address.as_deref(), Some("127.0.0.1:2306"));
    assert!(!parse_bans(&EMPTY_BANS.replace("IP Bans:", "")).complete);

    let duplicate = POPULATED_BANS.replace(
        "\n\nIP Bans:",
        &format!("\n0 {GUID} 0 duplicate reason\n\nIP Bans:"),
    );
    assert!(!parse_bans(&duplicate).complete);
}

#[test]
fn remove_ban_preflight_checks_current_id_and_guid() {
    let reply = format!(
        "GUID Bans:\n[#] [GUID] [Minutes left] [Reason]\n----------------------------------------\n4  {GUID} 0    test reason\n\nIP Bans:\n[#] [IP Address] [Minutes left] [Reason]\n----------------------------------------------"
    );
    let command = ArmaAdminCommand::RemoveBan {
        ban_id: 4,
        expected_guid: GUID.into(),
    };
    assert!(command.validate_preflight(&reply).is_ok());
    assert!(
        ArmaAdminCommand::RemoveBan {
            ban_id: 4,
            expected_guid: "abcdefabcdefabcdefabcdefabcdefab".into(),
        }
        .validate_preflight(&reply)
        .is_err()
    );
    assert!(
        ArmaAdminCommand::RemoveBan {
            ban_id: 99,
            expected_guid: GUID.into(),
        }
        .validate_preflight(&reply)
        .is_err()
    );
}

#[test]
fn mission_parser_returns_only_valid_templates_and_marks_partial_data_incomplete() {
    let parsed =
        parse_missions("Missions on server:\nmission_alpha.vr.pbo\nmission_beta.Altis.pbo");
    assert!(parsed.complete);
    assert_eq!(
        parsed.missions,
        vec![
            "mission_alpha.vr".to_owned(),
            "mission_beta.Altis".to_owned()
        ]
    );
    assert!(parse_missions(EMPTY_MISSIONS).complete);
    assert!(!parse_missions("Missions on server:\nmission_alpha.vr").complete);
    assert!(!parse_missions("Missions on server:\n../evil.Altis.pbo").complete);
    assert!(!parse_missions("Missions on server:\nmission.Altis.pbo\nmission.Altis.pbo").complete);
}

#[test]
fn change_mission_preflight_requires_a_fresh_available_template() {
    let command = ArmaAdminCommand::ChangeMission {
        template: "mission_alpha.VR".into(),
    };
    assert_eq!(command.required_preflight(), Some("missions"));
    assert!(
        command
            .validate_preflight("Missions on server:\nmission_alpha.vr.pbo")
            .is_ok()
    );
    assert!(
        command
            .validate_preflight("Missions on server:\nother.Altis.pbo")
            .is_err()
    );
    assert!(
        command
            .validate_preflight("Missions on server:\nmission_alpha.vr")
            .is_err()
    );
}
