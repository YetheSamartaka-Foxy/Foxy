#[derive(Clone, Copy, Debug, Default)]
pub(super) struct Proof {
    pub mission_seen: bool,
    pub content_isolated: bool,
    pub profile_readback: bool,
    pub mission_readback: bool,
    pub save_requested: bool,
    pub regular_client_present: bool,
}

pub(super) fn parse(text: &str, identity: &str, hostname: &str) -> Proof {
    let mut proof = Proof::default();
    for line in text.lines() {
        let Some((prefix, rest)) = line.trim().split_once('"') else {
            continue;
        };
        if !prefix
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b':' | b'.' | b' '))
        {
            continue;
        }
        let Some(message) = rest.strip_suffix('"') else {
            continue;
        };
        let fields = message.split('|').collect::<Vec<_>>();
        if fields.len() < 3 || fields[0] != "FOXY_NATIVE" || fields[1] != identity {
            continue;
        }
        if fields.len() == 3 && fields[2] == "save_requested" {
            proof.save_requested = true;
        }
        if fields.len() == 3 && fields[2] == "regular_client_present" {
            proof.regular_client_present = true;
        }
        if fields.len() != 8
            || fields[2] != "mission"
            || fields[3] != hostname
            || fields[4..]
                .iter()
                .any(|field| !matches!(*field, "true" | "false"))
        {
            continue;
        }
        proof.mission_seen = true;
        proof.content_isolated = fields[4] == "true" && fields[5] == "false";
        proof.profile_readback = fields[6] == "true";
        proof.mission_readback = fields[7] == "true";
    }
    proof
}

#[cfg(windows)]
pub(super) fn explicit_fixture_key_refusal(text: &str) -> bool {
    text.lines().any(|line| {
        let line = line.to_ascii_lowercase();
        line.contains("not signed by a key accepted by this server")
            && line.contains("foxy_native_alpha.pbo")
    })
}

#[cfg(windows)]
#[test]
fn key_refusal_requires_a_single_native_diagnostic_naming_the_actual_fixture() {
    assert!(explicit_fixture_key_refusal(
        "Warning Message: Files 'R:/Mods/RepoAlpha/addons/foxy_native_alpha.pbo' are not signed by a key accepted by this server."
    ));
    assert!(!explicit_fixture_key_refusal(
        "Loaded foxy_native_alpha.pbo\nOther files are not signed by a key accepted by this server."
    ));
    assert!(!explicit_fixture_key_refusal(
        "Signature check timed out for foxy_native_alpha.pbo"
    ));
    assert!(!explicit_fixture_key_refusal(
        "Files 'other.pbo' are not signed by a key accepted by this server."
    ));
}

#[test]
fn exact_native_markers_are_separate_from_script_echo_and_other_instance_output() {
    let text = "12:00:00 \"FOXY_NATIVE|FixtureAlpha|mission|MainServer|true|false|true|true\"\n12:00:00 \"FOXY_NATIVE|FixtureAlpha|save_requested\"\n";
    let proof = parse(text, "FixtureAlpha", "MainServer");
    assert!(
        proof.mission_seen
            && proof.content_isolated
            && proof.profile_readback
            && proof.mission_readback
            && proof.save_requested
    );
    assert!(!parse(text, "FixtureBeta", "MainServer").mission_seen);
    assert!(!parse(text, "FixtureAlpha", "OtherServer").mission_seen);
    assert!(
        !parse(
            "Error diag_log \"FOXY_NATIVE|FixtureAlpha|mission|MainServer|true|false|true|true\"",
            "FixtureAlpha",
            "MainServer"
        )
        .mission_seen
    );
    assert!(
        !parse(
            "12:00:00 diag_log \"FOXY_NATIVE|FixtureAlpha|save_requested\";",
            "FixtureAlpha",
            "MainServer"
        )
        .save_requested
    );
    assert!(
        !parse(
            &text.replace("|false|", "|true|"),
            "FixtureAlpha",
            "MainServer"
        )
        .content_isolated
    );
}
