use crate::types::{DLC_CODES, DlcContent, ProcessedMod};

/// Shaping options for the generated `-mod=` line.
#[derive(Debug, Clone, Copy)]
pub struct ModLineOptions<'a> {
    /// Path prefix for each mod folder, matching the server-side layout
    /// (e.g. `mods/` for `-mod=mods/@cba_a3`).
    pub prefix: &'a str,
    /// Include optional mods alongside the required ones.
    pub include_optional: bool,
}

/// Builds the `-mod=` launch parameter for a generated repository.
///
/// Creator DLC codes come first (Arma resolves them from the game install),
/// then the repository mod folders. Disabled and client-side mods are never
/// emitted: client-side mods are not meant to be loaded by a server.
pub fn build_mod_line(
    dlc_content: Option<&DlcContent>,
    mods: &[ProcessedMod],
    options: ModLineOptions<'_>,
) -> String {
    let mut entries: Vec<String> = Vec::new();

    if let Some(dlc) = dlc_content {
        for code in DLC_CODES {
            if dlc.is_enabled(code) {
                entries.push(code.to_string());
            }
        }
    }

    let prefix = normalize_prefix(options.prefix);
    for m in mods {
        if !m.enabled || m.client_side {
            continue;
        }
        if !m.is_required && !options.include_optional {
            continue;
        }
        entries.push(format!("{}{}", prefix, m.mod_name));
    }

    let mut line = String::from("-mod=");
    for entry in &entries {
        line.push_str(entry);
        line.push(';');
    }
    line
}

fn normalize_prefix(prefix: &str) -> String {
    let trimmed = prefix.trim().trim_end_matches(['/', '\\']);
    if trimmed.is_empty() {
        String::new()
    } else {
        format!("{}/", trimmed.replace('\\', "/"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Checksums;

    fn processed(name: &str, is_required: bool, enabled: bool, client_side: bool) -> ProcessedMod {
        ProcessedMod {
            mod_name: name.to_string(),
            checksums: Checksums::default(),
            files: Vec::new(),
            is_required,
            enabled,
            client_side,
        }
    }

    fn options(prefix: &str) -> ModLineOptions<'_> {
        ModLineOptions {
            prefix,
            include_optional: false,
        }
    }

    #[test]
    fn empty_repository_yields_bare_flag() {
        assert_eq!(build_mod_line(None, &[], options("")), "-mod=");
    }

    #[test]
    fn dlc_codes_precede_mod_folders() {
        let dlc = DlcContent {
            ws: true,
            gm: true,
            ..DlcContent::default()
        };
        let mods = [processed("@cba_a3", true, true, false)];
        assert_eq!(
            build_mod_line(Some(&dlc), &mods, options("mods")),
            "-mod=gm;ws;mods/@cba_a3;"
        );
    }

    #[test]
    fn prefix_is_normalized_to_forward_slashes() {
        let mods = [processed("@ace", true, true, false)];
        assert_eq!(
            build_mod_line(None, &mods, options("a3\\mods\\")),
            "-mod=a3/mods/@ace;"
        );
    }

    #[test]
    fn disabled_and_client_side_mods_are_skipped() {
        let mods = [
            processed("@cba_a3", true, true, false),
            processed("@disabled", true, false, false),
            processed("@client", true, true, true),
        ];
        assert_eq!(build_mod_line(None, &mods, options("")), "-mod=@cba_a3;");
    }

    #[test]
    fn optional_mods_are_opt_in() {
        let mods = [
            processed("@cba_a3", true, true, false),
            processed("@extra", false, true, false),
        ];
        assert_eq!(build_mod_line(None, &mods, options("")), "-mod=@cba_a3;");
        assert_eq!(
            build_mod_line(
                None,
                &mods,
                ModLineOptions {
                    prefix: "",
                    include_optional: true,
                },
            ),
            "-mod=@cba_a3;@extra;"
        );
    }
}
