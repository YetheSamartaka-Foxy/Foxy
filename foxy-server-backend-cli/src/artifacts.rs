//! Manifest writers shared by `create` (one repository) and `create-space`
//! (every repository of a space in one pass).

use anyhow::Result;
use std::path::Path;

use crate::cli::GenerationMode;
use crate::hash;
use crate::srf;
use crate::types::{ProcessedMod, RepoConfig};

pub fn mode_label(mode: GenerationMode) -> &'static str {
    match mode {
        GenerationMode::Foxy => "FoxyMode (BLAKE3)",
        GenerationMode::Swifty => "SwiftyMode (MD5, legacy)",
        GenerationMode::Hybrid => "HybridMode (BLAKE3 + MD5)",
    }
}

pub fn uses_swifty(mode: GenerationMode) -> bool {
    matches!(mode, GenerationMode::Swifty | GenerationMode::Hybrid)
}

pub fn uses_foxy(mode: GenerationMode) -> bool {
    matches!(mode, GenerationMode::Foxy | GenerationMode::Hybrid)
}

/// Write the per-mod manifests (`mod.srf` and/or `foxy_addon.json`) into
/// `<dir>/<mod>/` for every mod.
pub fn write_mod_manifests(mods: &[ProcessedMod], dir: &Path, mode: GenerationMode) -> Result<()> {
    if uses_swifty(mode) {
        for m in mods {
            srf::write_mod_srf(m, dir)?;
        }
    }
    if uses_foxy(mode) {
        for m in mods {
            srf::write_foxy_addon_json(m, dir)?;
        }
    }
    Ok(())
}

/// Write the repository-level manifests (`foxy_addons.json` and `repo.json`)
/// into `dir` and return the repository checksum published in `repo.json`.
pub fn write_repo_manifests(
    config: &RepoConfig,
    mods: &[ProcessedMod],
    dir: &Path,
    mode: GenerationMode,
    app_update_url: Option<&str>,
) -> Result<String> {
    let foxy_repo_checksum = uses_foxy(mode).then(|| hash::compute_foxy_repo_checksum(mods));
    if let Some(checksum) = &foxy_repo_checksum {
        srf::write_foxy_addons_json(mods, checksum, dir)?;
    }

    let repo_checksum = match mode {
        GenerationMode::Foxy => foxy_repo_checksum.expect("foxy checksum computed for FoxyMode"),
        GenerationMode::Swifty | GenerationMode::Hybrid => hash::compute_repo_checksum(mods),
    };
    let effective_app_update_url = app_update_url.or(config.app_update_url.as_deref());

    srf::write_repo_json(
        config,
        mods,
        &repo_checksum,
        dir,
        mode,
        effective_app_update_url,
    )?;

    Ok(repo_checksum)
}

/// Human-readable list of the artifact sets a mode produces, for summaries.
pub fn artifact_lines(mode: GenerationMode) -> Vec<&'static str> {
    let mut lines = Vec::new();
    if uses_foxy(mode) {
        lines.push("foxy_addon.json (per mod), foxy_addons.json, repo.json");
    }
    if uses_swifty(mode) {
        lines.push("mod.srf (per mod), repo.json");
    }
    lines
}
