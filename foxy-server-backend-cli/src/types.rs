use serde::{Deserialize, Serialize};

/// Holds the checksum(s) for a hashed item, depending on generation mode.
#[derive(Debug, Clone, Default)]
pub enum Checksums {
    /// Placeholder before checksums are computed. Panics if accessed.
    #[default]
    Pending,
    /// SwiftyMode: MD5 only.
    Md5(String),
    /// FoxyMode: BLAKE3 only.
    Blake3(String),
    /// HybridMode: both computed in a single I/O pass.
    Hybrid { md5: String, blake3: String },
}

impl Checksums {
    pub fn md5(&self) -> Option<&str> {
        match self {
            Checksums::Md5(s) | Checksums::Hybrid { md5: s, .. } => Some(s),
            Checksums::Blake3(_) => None,
            Checksums::Pending => panic!("checksum accessed before computation"),
        }
    }

    pub fn blake3(&self) -> Option<&str> {
        match self {
            Checksums::Blake3(s) | Checksums::Hybrid { blake3: s, .. } => Some(s),
            Checksums::Md5(_) => None,
            Checksums::Pending => panic!("checksum accessed before computation"),
        }
    }

    pub fn unwrap_md5(&self) -> &str {
        self.md5().expect("md5 checksum absent for current mode")
    }

    pub fn unwrap_blake3(&self) -> &str {
        self.blake3()
            .expect("blake3 checksum absent for current mode")
    }
}

/// A single contiguous byte range within a file (PBO entry or whole file).
#[derive(Debug, Clone)]
pub struct FilePart {
    pub path: String,
    pub checksums: Checksums,
    pub start: u64,
    pub length: u64,
}

/// A file within a mod folder, with its computed parts and checksum.
#[derive(Debug, Clone)]
pub struct ModFile {
    pub relative_path: String,
    pub checksums: Checksums,
    pub length: u64,
    pub parts: Vec<FilePart>,
    pub data_order: usize,
}

/// A processed mod (addon folder) with its files and computed checksum.
#[derive(Debug, Clone)]
pub struct ProcessedMod {
    pub mod_name: String,
    pub checksums: Checksums,
    pub files: Vec<ModFile>,
    pub is_required: bool,
    pub enabled: bool,
    pub client_side: bool,
}

// --- Config types (input JSON) ---

#[derive(Debug, Deserialize)]
pub struct RepoConfig {
    #[serde(rename = "repoName")]
    pub repo_name: String,
    #[serde(rename = "basePath")]
    pub base_path: String,
    #[serde(rename = "appUpdateUrl", default)]
    pub app_update_url: Option<String>,
    #[serde(rename = "requiredMods", default)]
    pub required_mods: Vec<ModRef>,
    #[serde(rename = "optionalMods", default)]
    pub optional_mods: Vec<ModRef>,
    #[serde(rename = "iconImagePath", default)]
    pub icon_image_path: String,
    #[serde(rename = "repoImagePath", default)]
    pub repo_image_path: String,
    #[serde(rename = "clientParameters", default)]
    pub client_parameters: String,
    #[serde(rename = "repoBasicAuthentication", default)]
    pub repo_basic_authentication: RepoBasicAuthentication,
    #[serde(default = "default_repo_version")]
    pub version: String,
    #[serde(default)]
    pub servers: Vec<ServerEntry>,
    #[serde(rename = "dlcContent", default)]
    pub dlc_content: Option<DlcContent>,
}

/// Arma 3 DLC suggestions published in `repo.json` as `dlcContent`. Config
/// authors may write either the object form (`{"gm": true}`) or a list of
/// codes (`["gm", "spe"]`); both normalize to the object form on output.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct DlcContent {
    pub csla: bool,
    pub ef: bool,
    pub gm: bool,
    pub rf: bool,
    pub spe: bool,
    pub vn: bool,
    pub ws: bool,
}

pub const DLC_CODES: [&str; 7] = ["csla", "ef", "gm", "rf", "spe", "vn", "ws"];

impl DlcContent {
    fn flag_mut(&mut self, code: &str) -> Option<&mut bool> {
        match code {
            "csla" => Some(&mut self.csla),
            "ef" => Some(&mut self.ef),
            "gm" => Some(&mut self.gm),
            "rf" => Some(&mut self.rf),
            "spe" => Some(&mut self.spe),
            "vn" => Some(&mut self.vn),
            "ws" => Some(&mut self.ws),
            _ => None,
        }
    }
}

impl<'de> Deserialize<'de> for DlcContent {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            Codes(Vec<String>),
            Flags(std::collections::BTreeMap<String, bool>),
        }

        let mut dlc = DlcContent::default();
        match Raw::deserialize(deserializer)? {
            Raw::Codes(codes) => {
                for code in codes {
                    let normalized = code.trim().to_ascii_lowercase();
                    match dlc.flag_mut(&normalized) {
                        Some(flag) => *flag = true,
                        None => return Err(unknown_dlc_code::<D>(&code)),
                    }
                }
            }
            Raw::Flags(flags) => {
                for (code, value) in flags {
                    let normalized = code.trim().to_ascii_lowercase();
                    match dlc.flag_mut(&normalized) {
                        Some(flag) => *flag = value,
                        None => return Err(unknown_dlc_code::<D>(&code)),
                    }
                }
            }
        }

        Ok(dlc)
    }
}

fn unknown_dlc_code<'de, D: serde::Deserializer<'de>>(code: &str) -> D::Error {
    serde::de::Error::custom(format!(
        "unknown dlcContent code \"{}\": expected one of {}",
        code,
        DLC_CODES.join(", ")
    ))
}

#[derive(Debug, Deserialize)]
pub struct ModRef {
    #[serde(rename = "modName")]
    pub mod_name: String,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    #[serde(rename = "clientSide", default)]
    pub client_side: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ServerEntry {
    pub name: String,
    pub address: String,
    pub port: String,
    #[serde(default)]
    pub password: String,
    #[serde(rename = "battleEye", default)]
    pub battle_eye: bool,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct RepoBasicAuthentication {
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub password: String,
}

// --- Output types for repo.json ---

#[derive(Debug, Serialize)]
pub struct RepoJson {
    #[serde(rename = "repoName")]
    pub repo_name: String,
    pub checksum: String,
    /// Present when FoxyMode or HybridMode is used. Indicates the Foxy protocol version.
    #[serde(rename = "foxyMode", skip_serializing_if = "Option::is_none")]
    pub foxy_mode: Option<String>,
    #[serde(rename = "requiredMods")]
    pub required_mods: Vec<ModEntry>,
    #[serde(rename = "optionalMods")]
    pub optional_mods: Vec<ModEntry>,
    #[serde(rename = "iconImagePath")]
    pub icon_image_path: String,
    #[serde(rename = "iconImageChecksum")]
    pub icon_image_checksum: String,
    #[serde(rename = "repoImagePath")]
    pub repo_image_path: String,
    #[serde(rename = "repoImageChecksum")]
    pub repo_image_checksum: String,
    #[serde(rename = "appUpdateUrl", skip_serializing_if = "Option::is_none")]
    pub app_update_url: Option<String>,
    #[serde(rename = "clientParameters")]
    pub client_parameters: String,
    #[serde(rename = "repoBasicAuthentication")]
    pub repo_basic_authentication: RepoBasicAuthentication,
    pub version: String,
    pub servers: Vec<ServerEntry>,
    #[serde(rename = "dlcContent", skip_serializing_if = "Option::is_none")]
    pub dlc_content: Option<DlcContent>,
}

#[derive(Debug, Serialize)]
pub struct ModEntry {
    #[serde(rename = "modName")]
    pub mod_name: String,
    #[serde(rename = "checkSum")]
    pub check_sum: String,
    pub enabled: bool,
    #[serde(rename = "clientSide", skip_serializing_if = "is_false")]
    pub client_side: bool,
}

// --- Output types for mod.srf ---

#[derive(Debug, Serialize)]
pub struct SrfManifest {
    #[serde(rename = "Name")]
    pub name: String,
    #[serde(rename = "Checksum")]
    pub checksum: String,
    #[serde(rename = "Files")]
    pub files: Vec<SrfFile>,
}

#[derive(Debug, Serialize)]
pub struct SrfFile {
    #[serde(rename = "Path")]
    pub path: String,
    #[serde(rename = "Checksum")]
    pub checksum: String,
    #[serde(rename = "Length")]
    pub length: u64,
    #[serde(rename = "Type")]
    pub file_type: String,
    #[serde(rename = "Parts")]
    pub parts: Vec<SrfPart>,
}

#[derive(Debug, Serialize)]
pub struct SrfPart {
    #[serde(rename = "Path")]
    pub path: String,
    #[serde(rename = "Checksum")]
    pub checksum: String,
    #[serde(rename = "Start")]
    pub start: u64,
    #[serde(rename = "Length")]
    pub length: u64,
}

// --- Output types for foxy_addon.json (per-mod, FoxyMode) ---

#[derive(Debug, Serialize)]
pub struct FoxyAddonJson {
    pub name: String,
    pub version: String,
    pub checksum: String,
    #[serde(rename = "hashAlgorithm")]
    pub hash_algorithm: String,
    pub files: Vec<FoxyAddonFile>,
}

#[derive(Debug, Serialize)]
pub struct FoxyAddonFile {
    pub path: String,
    pub checksum: String,
    pub length: u64,
    #[serde(rename = "fileType")]
    pub file_type: String,
    pub parts: Vec<FoxyAddonPart>,
}

#[derive(Debug, Serialize)]
pub struct FoxyAddonPart {
    pub path: String,
    pub checksum: String,
    pub start: u64,
    pub length: u64,
}

// --- Output type for foxy_addons.json (repo-level, FoxyMode) ---

#[derive(Debug, Serialize)]
pub struct FoxyAddonsJson {
    pub version: String,
    #[serde(rename = "hashAlgorithm")]
    pub hash_algorithm: String,
    pub checksum: String,
    #[serde(rename = "requiredMods")]
    pub required_mods: Vec<ModEntry>,
    #[serde(rename = "optionalMods")]
    pub optional_mods: Vec<ModEntry>,
}

/// A resolved mod reference after wildcard expansion.
#[derive(Debug, Clone)]
pub struct ResolvedMod {
    pub mod_name: String,
    pub source_path: std::path::PathBuf,
    pub is_required: bool,
    pub enabled: bool,
    pub client_side: bool,
}

/// A discovered file within a mod, ready for processing.
#[derive(Debug, Clone)]
pub struct DiscoveredFile {
    pub absolute_path: std::path::PathBuf,
    pub relative_path: String,
    pub file_size: u64,
}

fn default_enabled() -> bool {
    true
}

fn default_repo_version() -> String {
    "3.2.0.0".to_string()
}

fn is_false(value: &bool) -> bool {
    !*value
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Checksums accessors ─────────────────────────────────────────────

    #[test]
    fn md5_variant_returns_md5() {
        let cs = Checksums::Md5("ABC".to_string());
        assert_eq!(cs.md5(), Some("ABC"));
        assert_eq!(cs.blake3(), None);
    }

    #[test]
    fn blake3_variant_returns_blake3() {
        let cs = Checksums::Blake3("XYZ".to_string());
        assert_eq!(cs.blake3(), Some("XYZ"));
        assert_eq!(cs.md5(), None);
    }

    #[test]
    fn hybrid_variant_returns_both() {
        let cs = Checksums::Hybrid {
            md5: "MD5".to_string(),
            blake3: "B3".to_string(),
        };
        assert_eq!(cs.md5(), Some("MD5"));
        assert_eq!(cs.blake3(), Some("B3"));
    }

    #[test]
    #[should_panic(expected = "checksum accessed before computation")]
    fn pending_md5_panics() {
        Checksums::Pending.md5();
    }

    #[test]
    #[should_panic(expected = "checksum accessed before computation")]
    fn pending_blake3_panics() {
        Checksums::Pending.blake3();
    }

    #[test]
    fn unwrap_md5_returns_value() {
        let cs = Checksums::Md5("HASH".to_string());
        assert_eq!(cs.unwrap_md5(), "HASH");
    }

    #[test]
    #[should_panic(expected = "md5 checksum absent")]
    fn unwrap_md5_on_blake3_panics() {
        Checksums::Blake3("X".to_string()).unwrap_md5();
    }

    #[test]
    fn unwrap_blake3_returns_value() {
        let cs = Checksums::Blake3("B3HASH".to_string());
        assert_eq!(cs.unwrap_blake3(), "B3HASH");
    }

    #[test]
    #[should_panic(expected = "blake3 checksum absent")]
    fn unwrap_blake3_on_md5_panics() {
        Checksums::Md5("X".to_string()).unwrap_blake3();
    }

    #[test]
    fn default_is_pending() {
        let cs = Checksums::default();
        assert!(matches!(cs, Checksums::Pending));
    }

    // ── Serde defaults ──────────────────────────────────────────────────

    #[test]
    fn default_enabled_is_true() {
        assert!(default_enabled());
    }

    #[test]
    fn default_repo_version_value() {
        assert_eq!(default_repo_version(), "3.2.0.0");
    }

    // ── RepoConfig deserialization ──────────────────────────────────────

    #[test]
    fn repo_config_minimal() {
        let json = r#"{"repoName": "Test", "basePath": "/mods"}"#;
        let config: RepoConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.repo_name, "Test");
        assert_eq!(config.base_path, "/mods");
        assert!(config.required_mods.is_empty());
        assert!(config.optional_mods.is_empty());
        assert!(config.app_update_url.is_none());
        assert_eq!(config.version, "3.2.0.0");
    }

    #[test]
    fn repo_config_with_mods() {
        let json = r#"{
            "repoName": "Repo",
            "basePath": "/mods",
            "requiredMods": [{"modName": "@ace"}, {"modName": "@cba", "enabled": false}],
            "optionalMods": [{"modName": "@tfar", "clientSide": true}]
        }"#;
        let config: RepoConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.required_mods.len(), 2);
        assert!(config.required_mods[0].enabled);
        assert!(!config.required_mods[1].enabled);
        assert_eq!(config.optional_mods.len(), 1);
        assert!(config.optional_mods[0].client_side);
        assert!(!config.required_mods[0].client_side);
    }

    #[test]
    fn repo_config_with_app_update_url() {
        let json = r#"{
            "repoName": "Repo",
            "basePath": "/mods",
            "appUpdateUrl": "https://example.com/update/"
        }"#;
        let config: RepoConfig = serde_json::from_str(json).unwrap();
        assert_eq!(
            config.app_update_url,
            Some("https://example.com/update/".to_string())
        );
    }

    // ── dlcContent deserialization ──────────────────────────────────────

    #[test]
    fn repo_config_without_dlc_content_is_none() {
        let json = r#"{"repoName": "Test", "basePath": "/mods"}"#;
        let config: RepoConfig = serde_json::from_str(json).unwrap();
        assert!(config.dlc_content.is_none());
    }

    #[test]
    fn dlc_content_object_form_sets_named_flags() {
        let json = r#"{
            "repoName": "Test",
            "basePath": "/mods",
            "dlcContent": { "gm": true, "spe": true, "ws": false }
        }"#;
        let config: RepoConfig = serde_json::from_str(json).unwrap();
        let dlc = config.dlc_content.unwrap();
        assert!(dlc.gm);
        assert!(dlc.spe);
        assert!(!dlc.ws);
        assert!(!dlc.csla);
    }

    #[test]
    fn dlc_content_array_form_sets_listed_codes() {
        let json = r#"{
            "repoName": "Test",
            "basePath": "/mods",
            "dlcContent": ["GM", " spe "]
        }"#;
        let config: RepoConfig = serde_json::from_str(json).unwrap();
        let dlc = config.dlc_content.unwrap();
        assert!(dlc.gm);
        assert!(dlc.spe);
        assert!(!dlc.vn);
    }

    #[test]
    fn dlc_content_rejects_unknown_code() {
        let json = r#"{
            "repoName": "Test",
            "basePath": "/mods",
            "dlcContent": { "apex": true }
        }"#;
        let err = serde_json::from_str::<RepoConfig>(json)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("unknown dlcContent code"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn dlc_content_serializes_all_seven_flags() {
        let dlc = DlcContent {
            gm: true,
            ..DlcContent::default()
        };
        let json = serde_json::to_string(&dlc).unwrap();
        assert_eq!(
            json,
            r#"{"csla":false,"ef":false,"gm":true,"rf":false,"spe":false,"vn":false,"ws":false}"#
        );
    }

    // ── ServerEntry deserialization ──────────────────────────────────────

    #[test]
    fn server_entry_minimal() {
        let json = r#"{"name": "Main", "address": "1.2.3.4", "port": "2302"}"#;
        let entry: ServerEntry = serde_json::from_str(json).unwrap();
        assert_eq!(entry.name, "Main");
        assert_eq!(entry.port, "2302");
        assert!(entry.password.is_empty());
        assert!(!entry.battle_eye);
    }

    // ── RepoJson serialization ──────────────────────────────────────────

    #[test]
    fn repo_json_skips_none_foxy_mode() {
        let repo = RepoJson {
            repo_name: "Test".to_string(),
            checksum: "CS".to_string(),
            foxy_mode: None,
            required_mods: vec![],
            optional_mods: vec![],
            icon_image_path: "".to_string(),
            icon_image_checksum: "".to_string(),
            repo_image_path: "".to_string(),
            repo_image_checksum: "".to_string(),
            app_update_url: None,
            client_parameters: "".to_string(),
            repo_basic_authentication: RepoBasicAuthentication::default(),
            version: "3.2.0.0".to_string(),
            servers: vec![],
            dlc_content: None,
        };
        let json = serde_json::to_string(&repo).unwrap();
        assert!(!json.contains("foxyMode"));
        assert!(!json.contains("appUpdateUrl"));
    }

    #[test]
    fn repo_json_includes_dlc_content_when_present() {
        let repo = RepoJson {
            repo_name: "Test".to_string(),
            checksum: "CS".to_string(),
            foxy_mode: None,
            required_mods: vec![],
            optional_mods: vec![],
            icon_image_path: String::new(),
            icon_image_checksum: String::new(),
            repo_image_path: String::new(),
            repo_image_checksum: String::new(),
            app_update_url: None,
            client_parameters: String::new(),
            repo_basic_authentication: RepoBasicAuthentication::default(),
            version: "3.2.0.0".to_string(),
            servers: vec![],
            dlc_content: Some(DlcContent {
                spe: true,
                ..DlcContent::default()
            }),
        };
        let json = serde_json::to_string(&repo).unwrap();
        assert!(json.contains(r#""dlcContent":{"csla":false"#));
        assert!(json.contains(r#""spe":true"#));
    }

    #[test]
    fn repo_json_includes_foxy_mode_when_present() {
        let repo = RepoJson {
            repo_name: "Test".to_string(),
            checksum: "CS".to_string(),
            foxy_mode: Some("FoxyModeV1".to_string()),
            required_mods: vec![],
            optional_mods: vec![],
            icon_image_path: "".to_string(),
            icon_image_checksum: "".to_string(),
            repo_image_path: "".to_string(),
            repo_image_checksum: "".to_string(),
            app_update_url: Some("https://example.com".to_string()),
            client_parameters: "".to_string(),
            repo_basic_authentication: RepoBasicAuthentication::default(),
            version: "3.2.0.0".to_string(),
            servers: vec![],
            dlc_content: None,
        };
        let json = serde_json::to_string(&repo).unwrap();
        assert!(json.contains("FoxyModeV1"));
        assert!(json.contains("appUpdateUrl"));
    }
}
