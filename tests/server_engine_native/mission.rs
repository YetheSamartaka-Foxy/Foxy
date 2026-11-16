use sha1::{Digest, Sha1};
use std::fs;
use std::io::{self, Write};
use std::path::Path;

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
    {
        return Err(io::Error::other("Invalid native fixture identity"));
    }
    let (own, other) = match instance {
        "alpha" => ("FoxyNativeAlpha", "FoxyNativeBeta"),
        "beta" => ("FoxyNativeBeta", "FoxyNativeAlpha"),
        _ => return Err(io::Error::other("Unknown native fixture instance")),
    };
    let template = "foxy_native.vr".to_owned();
    let script = format!(
        "private _id = \"{identity}\";\nprivate _profile = profileNamespace getVariable [\"foxy_native_isolation\", \"\"];\nprivate _mission = missionProfileNamespace getVariable [\"foxy_native_isolation\", \"\"];\nprivate _own = isClass (configFile >> \"CfgPatches\" >> \"{own}\");\nprivate _other = isClass (configFile >> \"CfgPatches\" >> \"{other}\");\ndiag_log format [\"FOXY_NATIVE|%1|mission|%2|%3|%4|%5|%6\", _id, serverName, _own, _other, _profile isEqualTo _id, _mission isEqualTo _id];\nprofileNamespace setVariable [\"foxy_native_isolation\", _id];\nsaveProfileNamespace;\nmissionProfileNamespace setVariable [\"foxy_native_isolation\", _id];\nsaveMissionProfileNamespace;\ndiag_log format [\"FOXY_NATIVE|%1|save_requested\", _id];\n[_id] spawn {{ params [\"_id\"]; waitUntil {{ sleep 0.2; allPlayers findIf {{ !(_x isKindOf \"HeadlessClient_F\") }} >= 0 }}; sleep 3; if (allPlayers findIf {{ !(_x isKindOf \"HeadlessClient_F\") }} >= 0) then {{ diag_log format [\"FOXY_NATIVE|%1|regular_client_present\", _id]; }}; }};\n"
    );
    let description = b"onLoadName = \"Foxy native layout proof\";\nrespawn = 3;\nrespawnDelay = 1;\nclass Header { gameType = \"COOP\"; minPlayers = 1; maxPlayers = 1; };\n";
    let mission = b"version=12;\nclass Mission {\naddOns[]={\"A3_Characters_F_BLUFOR\",\"A3_Map_VR\"};\naddOnsAuto[]={\"A3_Characters_F_BLUFOR\",\"A3_Map_VR\"};\nrandomSeed=1;\nclass Intel { year=2035; month=6; day=24; hour=12; minute=0; };\nclass Groups { items=1; class Item0 { side=\"WEST\"; class Vehicles { items=1; class Item0 { position[]={500,0,500}; id=0; side=\"WEST\"; vehicle=\"B_Soldier_F\"; player=\"PLAYER COMMANDER\"; leader=1; skill=0.5; }; }; }; };\n};\n";
    let bytes = archive(
        &[
            ("mission.sqm", mission),
            ("description.ext", description),
            ("initServer.sqf", script.as_bytes()),
        ],
        None,
    );
    create_file(&missions.join(format!("{template}.pbo")), &bytes)?;
    fs::create_dir(content)?;
    let addons = content.join("addons");
    fs::create_dir(&addons)?;
    let config = format!(
        "class CfgPatches {{ class {own} {{ units[]={{}}; weapons[]={{}}; requiredVersion=0.1; requiredAddons[]={{}}; }}; }};\n"
    );
    let prefix = format!("foxy_native_{instance}");
    let bytes = archive(&[("config.cpp", config.as_bytes())], Some(&prefix));
    create_file(&addons.join(format!("{prefix}.pbo")), &bytes)?;
    Ok(Mission {
        template,
        identity: identity.to_owned(),
    })
}

pub(super) fn create_file(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

pub(super) fn archive(entries: &[(&str, &[u8])], prefix: Option<&str>) -> Vec<u8> {
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
fn generated_native_archives_have_complete_content_and_distinct_instance_patches() -> io::Result<()>
{
    use foxy_formats::{ContentFormat, PboFormat};
    let root = tempfile::tempdir()?;
    for name in ["alpha", "beta"] {
        let instance = root.path().join(name);
        fs::create_dir(&instance)?;
        let fixture = write(
            &instance,
            &instance.join("content"),
            name,
            &format!("Fixture_{name}"),
        )?;
        let path = instance.join(format!("{}.pbo", fixture.template));
        let parts = PboFormat.parse_parts(&path).map_err(io::Error::other)?;
        assert_eq!(parts.len(), 5);
        assert!(parts.iter().any(|part| part.path == "initServer.sqf"));
        let bytes = fs::read(path)?;
        assert_eq!(
            &bytes[bytes.len() - 20..],
            Sha1::digest(&bytes[..bytes.len() - 21]).as_slice()
        );
        let addon = instance.join(format!("content/addons/foxy_native_{name}.pbo"));
        assert_eq!(
            PboFormat
                .parse_parts(&addon)
                .map_err(io::Error::other)?
                .len(),
            3
        );
        assert!(write(&instance, &instance.join("other"), name, "bad\"identity").is_err());
    }
    Ok(())
}
