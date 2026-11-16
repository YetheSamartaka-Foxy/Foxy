use sha1::{Digest, Sha1};
use std::fs;
use std::io::{self, Write};
use std::path::Path;

pub(super) const TEMPLATE: &str = "foxy_configuration.vr";

pub(super) struct Mission {
    pub template: String,
    pub identity: String,
}

pub(super) fn write(
    missions: &Path,
    content: &Path,
    instance: &str,
    identity: &str,
) -> io::Result<Mission> {
    if identity.is_empty()
        || identity.len() > 128
        || !identity
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        || !instance.bytes().all(|byte| byte.is_ascii_alphanumeric())
    {
        return Err(io::Error::other("Invalid native configuration identity"));
    }
    let script = format!(
        "private _id = \"{identity}\";\n\
private _profile = profileNamespace getVariable [\"foxy_configuration_retained\", \"missing\"];\n\
private _mission = missionProfileNamespace getVariable [\"foxy_configuration_retained\", \"missing\"];\n\
private _group = createGroup west;\n\
private _unit = _group createUnit [\"B_Soldier_F\", [520, 0, 520], [], 0, \"NONE\"];\n\
_unit setSkill 1;\n\
diag_log format [\"FOXY_CONFIGURATION|%1|%2|%3|%4|%5|%6|%7|%8|%9|%10|end\", _id, toArray serverName, difficultyOption \"weaponCrosshair\", difficultyOption \"thirdPersonView\", difficultyOption \"tacticalPing\", paramsArray select 0, _unit skillFinal \"aimingAccuracy\", _unit skillFinal \"courage\", _profile, _mission];\n\
profileNamespace setVariable [\"foxy_configuration_retained\", \"foxy-managed-profile-retained\"];\n\
saveProfileNamespace;\n\
missionProfileNamespace setVariable [\"foxy_configuration_retained\", \"foxy-managed-mission-retained\"];\n\
saveMissionProfileNamespace;\n"
    );
    let description = b"onLoadName = \"Foxy configuration acceptance\";\nrespawn = 3;\nrespawnDelay = 1;\nclass Header { gameType = \"COOP\"; minPlayers = 1; maxPlayers = 1; };\nclass Params { class Probe { title = \"Probe\"; values[] = {17,23}; texts[] = {\"17\",\"23\"}; default = 17; }; };\n";
    let mission = b"version=12;\nclass Mission {\naddOns[]={\"A3_Characters_F_BLUFOR\",\"A3_Map_VR\"};\naddOnsAuto[]={\"A3_Characters_F_BLUFOR\",\"A3_Map_VR\"};\nrandomSeed=1;\nclass Intel { year=2035; month=6; day=24; hour=12; minute=0; };\nclass Groups { items=1; class Item0 { side=\"WEST\"; class Vehicles { items=1; class Item0 { position[]={500,0,500}; id=0; side=\"WEST\"; vehicle=\"B_Soldier_F\"; player=\"PLAYER COMMANDER\"; leader=1; skill=0.5; }; }; }; };\n};\n";
    let bytes = archive(
        &[
            ("mission.sqm", mission),
            ("description.ext", description),
            ("initServer.sqf", script.as_bytes()),
        ],
        None,
    );
    create_file(&missions.join(format!("{TEMPLATE}.pbo")), &bytes)?;
    fs::create_dir(content)?;
    let addons = content.join("addons");
    fs::create_dir(&addons)?;
    let patch = format!("FoxyConfiguration_{instance}");
    let config = format!(
        "class CfgPatches {{ class {patch} {{ units[]={{}}; weapons[]={{}}; requiredVersion=0.1; requiredAddons[]={{}}; }}; }};\n"
    );
    let prefix = format!("foxy_configuration_{instance}");
    let bytes = archive(&[("config.cpp", config.as_bytes())], Some(&prefix));
    create_file(&addons.join(format!("{prefix}.pbo")), &bytes)?;
    Ok(Mission {
        template: TEMPLATE.to_owned(),
        identity: identity.to_owned(),
    })
}

fn create_file(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

fn archive(entries: &[(&str, &[u8])], prefix: Option<&str>) -> Vec<u8> {
    let mut bytes = vec![0];
    bytes.extend_from_slice(b"sreV");
    bytes.extend_from_slice(&[0; 16]);
    if let Some(prefix) = prefix {
        bytes.extend_from_slice(b"prefix\0");
        bytes.extend_from_slice(prefix.as_bytes());
        bytes.push(0);
    }
    bytes.push(0);
    for (name, data) in entries {
        bytes.extend_from_slice(name.as_bytes());
        bytes.push(0);
        bytes.extend_from_slice(&[0; 16]);
        bytes.extend_from_slice(&(data.len() as u32).to_le_bytes());
    }
    bytes.extend_from_slice(&[0; 21]);
    for (_, data) in entries {
        bytes.extend_from_slice(data);
    }
    let checksum = Sha1::digest(&bytes);
    bytes.push(0);
    bytes.extend_from_slice(&checksum);
    bytes
}

#[test]
fn generated_configuration_mission_is_a_complete_archive() -> io::Result<()> {
    use foxy_formats::{ContentFormat, PboFormat};
    let root = tempfile::tempdir()?;
    let fixture = write(
        root.path(),
        &root.path().join("content"),
        "alpha",
        "ConfigA_1",
    )?;
    let path = root.path().join(format!("{}.pbo", fixture.template));
    let parts = PboFormat.parse_parts(&path).map_err(io::Error::other)?;
    assert!(parts.iter().any(|part| part.path == "initServer.sqf"));
    assert!(write(root.path(), &root.path().join("other"), "alpha", "bad\"id").is_err());
    Ok(())
}
