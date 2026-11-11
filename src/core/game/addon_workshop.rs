use std::fs::{self, File};
use std::io::Read;
use std::path::Path;
use std::time::Duration;

use super::workshop;

const MAX_METADATA_BYTES: u64 = 64 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AddonWorkshop {
    Steam(u32),
    Reforger,
}

impl AddonWorkshop {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Steam(_) => "Open on Steam Workshop",
            Self::Reforger => "Open on Arma Reforger Workshop",
        }
    }

    pub(crate) fn resolve(
        self,
        addon_name: &str,
        local_path: Option<&Path>,
        repository_url: Option<&str>,
    ) -> Option<String> {
        if let Some(path) = local_path {
            if let Self::Steam(app_id) = self {
                let canonical = path.canonicalize().ok();
                for candidate in [Some(path), canonical.as_deref()].into_iter().flatten() {
                    if let Some(id) =
                        workshop::workshop_item_id_from_path(&candidate.to_string_lossy(), app_id)
                            .and_then(|id| steam_id(&id))
                    {
                        return Some(workshop::workshop_url(&id));
                    }
                }
            }
            for file in self.metadata_files() {
                if let Some(raw) = File::open(path.join(file)).ok().and_then(read_metadata)
                    && let Some(url) = self.metadata_url(file, &raw)
                {
                    return Some(url);
                }
            }
            if self == Self::Reforger {
                let mut projects: Vec<_> = fs::read_dir(path)
                    .ok()
                    .into_iter()
                    .flatten()
                    .flatten()
                    .map(|entry| entry.path())
                    .filter(|path| {
                        path.extension()
                            .is_some_and(|ext| ext.eq_ignore_ascii_case("gproj"))
                    })
                    .collect();
                projects.sort();
                for project in projects {
                    if let Some(raw) = File::open(project).ok().and_then(read_metadata)
                        && let Some(url) = self.metadata_url("addon.gproj", &raw)
                    {
                        return Some(url);
                    }
                }
                if let Some(id) = path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .and_then(reforger_folder_id)
                {
                    return Some(reforger_url(&id));
                }
            }
        }
        if self == Self::Reforger
            && let Some(id) = reforger_folder_id(addon_name)
        {
            return Some(reforger_url(&id));
        }
        let repository_url = repository_url?;
        let client = crate::core::utils::proxy::blocking_client_builder()
            .connect_timeout(Duration::from_secs(3))
            .timeout(Duration::from_secs(5))
            .build()
            .ok()?;
        for file in self.metadata_files() {
            let url = metadata_request_url(repository_url, addon_name, file)?;
            let Some(response) = client
                .get(url)
                .send()
                .ok()
                .and_then(|response| response.error_for_status().ok())
            else {
                continue;
            };
            if let Some(raw) = read_metadata(response)
                && let Some(url) = self.metadata_url(file, &raw)
            {
                return Some(url);
            }
        }
        None
    }

    fn metadata_files(self) -> &'static [&'static str] {
        match self {
            Self::Steam(_) => &["meta.cpp"],
            Self::Reforger => &["addon.gproj", "ServerData.json", "meta"],
        }
    }

    fn metadata_url(self, file: &str, raw: &str) -> Option<String> {
        match self {
            Self::Steam(_) => {
                let id = config_field(raw, "publishedid", 0, true)?;
                Some(workshop::workshop_url(&steam_id(&id)?))
            }
            Self::Reforger => {
                let id = if file.ends_with(".gproj") {
                    config_field(raw, "GUID", 1, false)?
                } else {
                    let value: serde_json::Value =
                        serde_json::from_str(raw.trim_start_matches('\u{feff}')).ok()?;
                    let value = if file == "meta" {
                        value.get("meta")?
                    } else {
                        &value
                    };
                    value.get("id")?.as_str()?.to_string()
                };
                Some(reforger_url(&reforger_id(&id)?))
            }
        }
    }
}

fn read_metadata(reader: impl Read) -> Option<String> {
    let mut bytes = Vec::new();
    reader
        .take(MAX_METADATA_BYTES + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() as u64 > MAX_METADATA_BYTES {
        return None;
    }
    String::from_utf8(bytes).ok()
}

fn metadata_request_url(base: &str, addon: &str, file: &str) -> Option<reqwest::Url> {
    if addon.is_empty() || matches!(addon, "." | "..") || addon.contains(['/', '\\']) {
        return None;
    }
    let mut url = reqwest::Url::parse(base).ok()?;
    if !matches!(url.scheme(), "http" | "https") {
        return None;
    }
    url.set_query(None);
    url.set_fragment(None);
    url.path_segments_mut()
        .ok()?
        .pop_if_empty()
        .push(addon)
        .push(file);
    Some(url)
}

fn steam_id(raw: &str) -> Option<String> {
    if !raw.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    raw.parse::<u64>()
        .ok()
        .filter(|id| *id > 0)
        .map(|id| id.to_string())
}

fn reforger_id(raw: &str) -> Option<String> {
    (raw.len() == 16 && raw.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .then(|| raw.to_ascii_uppercase())
}

fn reforger_folder_id(name: &str) -> Option<String> {
    reforger_id(name).or_else(|| reforger_id(name.rsplit_once('_')?.1))
}

fn reforger_url(id: &str) -> String {
    format!("https://reforger.armaplatform.com/workshop/{id}")
}

fn config_field(raw: &str, key: &str, target_depth: usize, assignment: bool) -> Option<String> {
    let mut chars = raw.trim_start_matches('\u{feff}').chars().peekable();
    let mut tokens = Vec::new();
    while let Some(ch) = chars.next() {
        match ch {
            '/' if chars.peek() == Some(&'/') => {
                chars.next();
                for ch in chars.by_ref() {
                    if ch == '\n' {
                        break;
                    }
                }
            }
            '/' if chars.peek() == Some(&'*') => {
                chars.next();
                while let Some(ch) = chars.next() {
                    if ch == '*' && chars.peek() == Some(&'/') {
                        chars.next();
                        break;
                    }
                }
            }
            '"' => {
                let mut token = String::new();
                let mut closed = false;
                while let Some(ch) = chars.next() {
                    if ch == '"' {
                        closed = true;
                        break;
                    }
                    if ch == '\\' {
                        token.push(chars.next()?);
                    } else {
                        token.push(ch);
                    }
                }
                if !closed {
                    return None;
                }
                tokens.push((token, true));
            }
            ch if ch.is_ascii_alphanumeric() || ch == '_' => {
                let mut token = ch.to_string();
                while chars
                    .peek()
                    .is_some_and(|ch| ch.is_ascii_alphanumeric() || *ch == '_')
                {
                    token.push(chars.next()?);
                }
                tokens.push((token, false));
            }
            ch if !ch.is_whitespace() => tokens.push((ch.to_string(), false)),
            _ => {}
        }
    }
    let mut depth = 0usize;
    for (index, (token, quoted)) in tokens.iter().enumerate() {
        if *quoted {
            continue;
        }
        match token.as_str() {
            "{" => depth += 1,
            "}" => depth = depth.saturating_sub(1),
            _ if depth == target_depth && token.eq_ignore_ascii_case(key) => {
                let mut cursor = index + 1;
                if assignment {
                    if tokens.get(cursor)?.0 != "=" {
                        continue;
                    }
                    cursor += 1;
                }
                let (value, quoted) = tokens.get(cursor)?;
                if assignment {
                    if *quoted || tokens.get(cursor + 1)?.0 != ";" {
                        continue;
                    }
                } else if !quoted {
                    continue;
                }
                return Some(value.clone());
            }
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steam_metadata_ignores_comments_strings_and_nested_keys() {
        let raw = r#"// publishedid = 111;
            /* publishedid = 222; */
            name = "publishedid = 333;";
            class Nested { publishedid = 444; };
            unpublishedid = 555;
            PublishedId /* comment */ = 000123456789;"#;
        assert_eq!(
            AddonWorkshop::Steam(107410).metadata_url("meta.cpp", raw),
            Some(workshop::workshop_url("123456789"))
        );
    }

    #[test]
    fn steam_metadata_rejects_invalid_ids() {
        for value in [
            "0",
            "18446744073709551616",
            "-123",
            "123.4",
            "\"123\"",
            "123abc",
        ] {
            assert!(
                AddonWorkshop::Steam(107410)
                    .metadata_url("meta.cpp", &format!("publishedid = {value};"))
                    .is_none(),
                "{value}"
            );
        }
    }

    #[test]
    fn reforger_metadata_requires_a_guid_and_ignores_nested_fields() {
        let raw = r#"GameProject { ID "ExampleAddon" Nested { GUID "1111111111111111" } GUID "abcdef1234567890" }"#;
        assert_eq!(
            AddonWorkshop::Reforger.metadata_url("addon.gproj", raw),
            Some(reforger_url("ABCDEF1234567890"))
        );
        assert!(
            AddonWorkshop::Reforger
                .metadata_url("addon.gproj", "GameProject { ID \"ExampleAddon\" }")
                .is_none()
        );
        assert!(
            AddonWorkshop::Reforger
                .metadata_url("addon.gproj", "GameProject { GUID \"not-a-guid\" }")
                .is_none()
        );
    }

    #[test]
    fn reforger_metadata_reads_bom_json() {
        for (file, raw) in [
            ("ServerData.json", "\u{feff}{\"id\":\"abcdef1234567890\"}"),
            ("meta", "\u{feff}{\"meta\":{\"id\":\"abcdef1234567890\"}}"),
        ] {
            assert_eq!(
                AddonWorkshop::Reforger.metadata_url(file, raw),
                Some(reforger_url("ABCDEF1234567890"))
            );
            assert!(
                AddonWorkshop::Reforger
                    .metadata_url(file, "{broken")
                    .is_none()
            );
        }
    }

    #[test]
    fn folder_fallback_requires_a_hex_guid() {
        assert_eq!(
            reforger_folder_id("@mod_01_abcdef1234567890"),
            Some("ABCDEF1234567890".to_string())
        );
        for name in [
            "ExampleAddon",
            "@mod_01",
            "ABCDEF123456789",
            "ABCDEFGHIJKLMNOP",
        ] {
            assert!(reforger_folder_id(name).is_none());
        }
    }

    #[test]
    fn metadata_urls_preserve_repository_path_and_encode_addon_names() {
        assert_eq!(
            metadata_request_url(
                "https://repo.example.invalid:8080/MainRepo",
                "@mod_01 #?%",
                "meta.cpp"
            )
            .unwrap()
            .as_str(),
            "https://repo.example.invalid:8080/MainRepo/@mod_01%20%23%3F%25/meta.cpp"
        );
        for name in ["..", "../@mod_01", "@mod_01\\meta.cpp"] {
            assert!(
                metadata_request_url("https://repo.example.invalid/MainRepo/", name, "meta.cpp")
                    .is_none()
            );
        }
        assert!(metadata_request_url("file:///R:/Mods/", "@mod_01", "meta.cpp").is_none());
    }

    #[test]
    fn metadata_reads_are_bounded() {
        assert!(read_metadata(&b"publishedid = 123;"[..]).is_some());
        assert!(read_metadata(vec![b'x'; MAX_METADATA_BYTES as usize + 1].as_slice()).is_none());
    }

    #[test]
    fn steam_paths_use_the_configured_app_id() {
        let path = Path::new("R:/Steam/steamapps/workshop/content/123456/987654321/mod.pack");
        assert_eq!(
            AddonWorkshop::Steam(123456).resolve("mod.pack", Some(path), None),
            Some(workshop::workshop_url("987654321"))
        );
        assert!(
            AddonWorkshop::Steam(107410)
                .resolve("mod.pack", Some(path), None)
                .is_none()
        );
    }

    #[test]
    fn local_metadata_falls_back_to_server_data() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("ServerData.json"),
            "\u{feff}{\"id\":\"ABCDEF1234567890\"}",
        )
        .unwrap();
        assert_eq!(
            AddonWorkshop::Reforger.resolve("@mod_01", Some(dir.path()), None),
            Some(reforger_url("ABCDEF1234567890"))
        );
    }

    #[test]
    fn local_metadata_handles_other_project_names_and_invalid_preferred_files() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("addon.gproj"),
            "GameProject { ID \"ExampleAddon\" }",
        )
        .unwrap();
        fs::write(dir.path().join("ServerData.json"), "{broken").unwrap();
        fs::write(
            dir.path().join("meta"),
            "{\"meta\":{\"id\":\"abcdef1234567890\"}}",
        )
        .unwrap();
        assert_eq!(
            AddonWorkshop::Reforger.resolve("@mod_01", Some(dir.path()), None),
            Some(reforger_url("ABCDEF1234567890"))
        );
        fs::remove_file(dir.path().join("meta")).unwrap();
        fs::write(
            dir.path().join("example.gproj"),
            "GameProject { GUID \"ABCDEF1234567890\" }",
        )
        .unwrap();
        assert_eq!(
            AddonWorkshop::Reforger.resolve("@mod_01", Some(dir.path()), None),
            Some(reforger_url("ABCDEF1234567890"))
        );
    }

    #[test]
    fn remote_metadata_is_a_fallback_for_missing_local_files() {
        use std::io::Write;
        use std::net::TcpListener;

        let server = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}/MainRepo/", server.local_addr().unwrap());
        let worker = std::thread::spawn(move || {
            for file in ["addon.gproj", "ServerData.json"] {
                let (mut stream, _) = server.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                let mut request = [0u8; 4096];
                let count = stream.read(&mut request).unwrap();
                assert!(
                    String::from_utf8_lossy(&request[..count])
                        .starts_with(&format!("GET /MainRepo/@mod_01/{file} HTTP/1.1"))
                );
                let (status, body) = if file == "addon.gproj" {
                    ("404 Not Found", "")
                } else {
                    ("200 OK", "\u{feff}{\"id\":\"abcdef1234567890\"}")
                };
                write!(
                    stream,
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .unwrap();
            }
        });
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            AddonWorkshop::Reforger.resolve("@mod_01", Some(dir.path()), Some(&base)),
            Some(reforger_url("ABCDEF1234567890"))
        );
        worker.join().unwrap();
    }

    #[test]
    fn module_dispatch_uses_steam_settings_and_reforger_provider() {
        use crate::core::game::{
            GameModule, arma3::Arma3Module, generic_game::GenericGameModule,
            reforger::ReforgerModule,
        };
        use crate::ui::types::SettingsViewState;

        let mut settings = SettingsViewState::default();
        assert_eq!(
            Arma3Module.addon_workshop(&settings),
            Some(AddonWorkshop::Steam(107410))
        );
        assert_eq!(
            ReforgerModule.addon_workshop(&settings),
            Some(AddonWorkshop::Reforger)
        );
        settings.generic_steam_app_id = String::new();
        assert_eq!(GenericGameModule.addon_workshop(&settings), None);
        settings.generic_steam_app_id = "123456".to_string();
        assert_eq!(
            GenericGameModule.addon_workshop(&settings),
            Some(AddonWorkshop::Steam(123456))
        );
    }
}
