use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use foxy_server_backend_cli::{PlayerFile, PlayerMod};

use super::super::content_staging::{ContentStageManifest, StageEntryKind, parse_relative};
use super::{
    ContentStageControl, Error, PlayerAddonReview, PlayerPublicationRequest, ServerAddonRole,
};

pub(super) fn project(
    request: &PlayerPublicationRequest,
    stage: &Path,
    manifest: &ContentStageManifest,
    control: &ContentStageControl,
) -> Result<(Vec<PlayerMod>, Vec<PlayerAddonReview>), Error> {
    let roles = manifest
        .bindings
        .bindings
        .iter()
        .map(|item| (item.profile.name.as_str(), item.binding.role))
        .collect::<BTreeMap<_, _>>();
    let mut chosen = BTreeSet::new();
    for name in &request.recommendations {
        if roles.get(name.as_str()) != Some(&ServerAddonRole::Optional)
            || !chosen.insert(name.as_str())
        {
            return Err(Error::Invalid(
                "Recommendations must name distinct optional profiles from the selected record."
                    .into(),
            ));
        }
    }

    let mut entries = BTreeMap::<&str, Vec<_>>::new();
    for entry in &manifest.entries {
        control.checkpoint()?;
        if entry.kind == StageEntryKind::File
            && let Some((directory, _)) = entry
                .path
                .strip_prefix("addons/")
                .and_then(|path| path.split_once('/'))
        {
            entries.entry(directory).or_default().push(entry);
        }
    }
    let mut mods = Vec::new();
    let mut reviews = Vec::new();
    for source in &manifest.sources {
        control.checkpoint()?;
        let Some(name) = source.destination.strip_prefix("addons/") else {
            continue;
        };
        let role = *roles
            .get(source.profile_name.as_str())
            .ok_or(Error::Corrupt)?;
        let selected = match role {
            ServerAddonRole::Required => true,
            ServerAddonRole::Optional => chosen.contains(source.profile_name.as_str()),
            ServerAddonRole::ServerOnly => false,
        };
        let prefix = format!("{}/", source.destination);
        let candidates = entries.get(name).map(Vec::as_slice).unwrap_or_default();
        let mut files = Vec::new();
        if role != ServerAddonRole::ServerOnly {
            for entry in candidates {
                let relative = entry.path.strip_prefix(&prefix).ok_or(Error::Corrupt)?;
                if allowed_player_file(relative, request.include_public_keys) {
                    files.push(PlayerFile {
                        relative_path: parse_relative(relative)?,
                        bytes: entry.bytes,
                        blake3: entry.blake3.clone().ok_or(Error::Corrupt)?,
                    });
                }
            }
        }
        reviews.push(PlayerAddonReview {
            profile: source.profile_name.clone(),
            name: name.into(),
            role,
            selected,
            files: files.len() as u64,
            bytes: files.iter().map(|file| file.bytes).sum(),
            excluded_files: (candidates.len() - files.len()) as u64,
        });
        if selected {
            if name.contains(';')
                || [
                    "repo.json",
                    "foxy_addons.json",
                    "server_mod_line.txt",
                    "manifest.json",
                ]
                .iter()
                .any(|reserved| name.eq_ignore_ascii_case(reserved))
            {
                return Err(Error::Invalid("A player addon name conflicts with a publication artifact or launch separator.".into()));
            }
            if !files.iter().any(|file| {
                file.relative_path
                    .extension()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("pbo"))
            }) {
                return Err(Error::Invalid(
                    "A selected player addon has no publishable PBO payload.".into(),
                ));
            }
            mods.push(PlayerMod {
                name: name.into(),
                required: role == ServerAddonRole::Required,
                root: stage.join(parse_relative(&source.destination)?),
                files,
            });
        }
    }
    Ok((mods, reviews))
}

fn allowed_player_file(path: &str, include_public_keys: bool) -> bool {
    let (parent, file) = path.rsplit_once('/').unwrap_or(("", path));
    let extension = file
        .rsplit_once('.')
        .map(|(_, extension)| extension)
        .unwrap_or("");
    ((parent.is_empty() || parent.eq_ignore_ascii_case("addons"))
        && (extension.eq_ignore_ascii_case("pbo") || extension.eq_ignore_ascii_case("bisign")))
        || (include_public_keys
            && (parent.is_empty() || parent.eq_ignore_ascii_case("keys"))
            && extension.eq_ignore_ascii_case("bikey"))
}

#[cfg(test)]
mod tests {
    use super::allowed_player_file;

    #[test]
    fn only_player_archives_signatures_and_explicit_public_keys_are_eligible() {
        for path in [
            "radio.PBO",
            "addons/radio.pbo",
            "AddOns/radio.pbo.alpha.bisign",
        ] {
            assert!(allowed_player_file(path, false), "{path}");
        }
        for path in [
            "rcon.cfg",
            "server.json",
            "userconfig/secret.pbo",
            "optionals/extra.pbo",
            "addons/secret/payload.pbo",
            "keys/sign.biprivatekey",
            "addons/foxy_addon.json",
        ] {
            assert!(!allowed_player_file(path, true), "{path}");
        }
        assert!(!allowed_player_file("keys/sign.bikey", false));
        assert!(allowed_player_file("keys/sign.bikey", true));
    }
}
