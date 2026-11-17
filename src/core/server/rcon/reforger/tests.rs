use super::*;

const IDENTITY: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAE=";
const OTHER_IDENTITY: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAF=";

#[test]
fn command_wire_text_and_permissions_follow_game_roles() {
    assert_eq!(
        ReforgerAdminCommand::Players.wire_text().unwrap(),
        "#players"
    );
    assert_eq!(ReforgerAdminCommand::Logout.wire_text().unwrap(), "@logout");
    assert!(!ReforgerAdminCommand::Players.requires_admin());
    assert!(!ReforgerAdminCommand::Logout.requires_admin());
    assert!(ReforgerAdminCommand::ReloadScenario.requires_admin());
    assert_eq!(
        ReforgerAdminCommand::Bans { page: 1 }.wire_text().unwrap(),
        "#ban list 1"
    );
    assert_eq!(
        ReforgerAdminCommand::ReloadScenario.wire_text().unwrap(),
        "#restart"
    );
    assert_eq!(
        ReforgerAdminCommand::Ban {
            identity: IDENTITY.into(),
            duration_seconds: 0,
            reason: "test reason".into()
        }
        .wire_text()
        .unwrap(),
        format!("#ban create {IDENTITY} 0 test reason")
    );
}

#[test]
fn command_validation_rejects_bad_pages_identities_and_multiline_reasons() {
    assert!(ReforgerAdminCommand::Bans { page: 0 }.wire_text().is_err());
    assert!(
        ReforgerAdminCommand::RemoveBan {
            identity: "123456789".into()
        }
        .wire_text()
        .is_err()
    );
    assert!(
        ReforgerAdminCommand::RemoveBan {
            identity: "01234567-89ab-cdef-0123-456789abcdef".into()
        }
        .wire_text()
        .is_err()
    );
    assert!(
        ReforgerAdminCommand::Ban {
            identity: IDENTITY.into(),
            duration_seconds: 30,
            reason: "first\nsecond".into()
        }
        .wire_text()
        .is_err()
    );
}

#[test]
fn serde_commands_reject_unknown_fields() {
    assert!(serde_json::from_str::<ReforgerAdminCommand>(r#"{"kind":"kick","player_id":1,"expected_identity":"AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAE=","extra":true}"#).is_err());
}

#[test]
fn kick_preflight_checks_observed_identity_without_claiming_complete_roster() {
    let command = ReforgerAdminCommand::Kick {
        player_id: 7,
        expected_identity: IDENTITY.into(),
    };
    let transcript = "Processing Command: #players\nPlayers on server: [Player#] ; [Player UID] ; [Player Name]\n[Player#7] ; [AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAE=] ; [PlayerAlpha]";
    assert_eq!(command.required_preflight(), Some("#players"));
    assert!(command.validate_preflight(transcript).is_ok());
    assert!(!parse_players(transcript).complete);
    let changed = transcript.replace(IDENTITY, OTHER_IDENTITY);
    assert!(command.validate_preflight(&changed).is_err());
}

#[test]
fn native_ban_no_results_is_complete_but_header_only_is_not() {
    let empty = parse_bans(
        "Processing Command: #ban list\nServer has no bans to list.",
        1,
    );
    assert!(empty.complete);
    assert!(empty.bans.is_empty());
    assert!(empty.raw.contains("Server has no bans to list."));

    let header = parse_bans(
        "Processing Command: #ban list\nBanID ; Player UID ; Duration",
        1,
    );
    assert!(!header.complete);
    assert!(header.bans.is_empty());
}

#[test]
fn rejection_detection_uses_only_the_observed_unknown_command_marker() {
    assert!(response_rejected("unknown command 'commands'"));
    assert!(response_rejected("Failed to create ban."));
    assert!(response_rejected("missing permission 'restart'"));
    assert!(response_rejected("missing permission 'players'"));
    assert!(!response_rejected("missing permission ''"));
    assert!(!response_rejected(
        "Processing Command: #players\nPlayers on server:"
    ));
    assert!(response_rejected(
        "error 'ban create AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAE= 120 FoxyAcceptance'"
    ));
    assert!(!response_rejected("Server has no bans to list."));
    assert!(!response_rejected("Processing Command: #players"));
}

#[test]
fn player_transcript_extracts_rows_and_keeps_raw_incomplete() {
    let transcript = "Processing Command: #players\nPlayers on server: [Player#] ; [Player UID] ; [Player Name]\n[Player#7] ; [AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAE=] ; [PlayerAlpha]";
    let roster = parse_players(transcript);
    assert!(!roster.complete);
    assert_eq!(
        roster.players,
        vec![ReforgerPlayer {
            player_id: 7,
            identity: Some(IDENTITY.into()),
            name: Some("PlayerAlpha".into()),
        }]
    );
    assert_eq!(roster.raw, transcript);
}
