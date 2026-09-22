//! `create-space`: generate every repository of a repository space and the
//! `repository_space.json` that links them in one pass.
//!
//! The space config points at the ordinary per-repository config files that
//! `create` consumes, so a space is a thin layer over the existing generator.
//! The layouts differ only in where mod folders live on disk:
//!
//! - `copy`: every repository gets its own copy of every mod (what running
//!   `create` per repository produces).
//! - `pool`: each distinct mod is copied once into a shared pool folder and
//!   each repository holds a relative symlink to it. Repositories of one space
//!   share a single download folder on the client, so a mod name maps to one
//!   content anyway; the pool just mirrors that on the server.
//! - `link`: each repository holds a symlink straight to the source folder and
//!   nothing is copied. The per-mod manifests are then written into the source
//!   folders, and any later change there silently invalidates the published
//!   checksums.
//!
//! In every layout the same mod name must hash identically across the
//! repositories that list it; otherwise clients would fight over the shared
//! folder, so the generator refuses.

use anyhow::{Context, Result, bail};
use indicatif::ProgressBar;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};
use std::time::Instant;

use crate::cli::{GenerationMode, SpaceLayout};
use crate::published;
use crate::types::{Checksums, ProcessedMod, RepoConfig, ResolvedMod};
use crate::{
    KeyCollectionRequest, artifacts, config, hash, keys, mod_line, mod_line_files, planner, srf,
};

mod cleanup;

pub const SPACE_MANIFEST: &str = "repository_space.json";
const DEFAULT_POOL_DIR: &str = "pool";
const DEFAULT_KEYS_DIR: &str = "keys";

// ---------------------------------------------------------------------------
// Input config
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct SpaceConfig {
    #[serde(alias = "spaceName")]
    pub name: String,
    /// Public URL the repository folders are served under; each repository
    /// without an explicit `address` gets `<baseUrl>/<folder>/`.
    #[serde(rename = "baseUrl", default)]
    pub base_url: String,
    #[serde(rename = "appUpdateUrl", default)]
    pub app_update_url: Option<String>,
    #[serde(rename = "iconImagePath", default)]
    pub icon_image_path: String,
    #[serde(rename = "repoImagePath", default)]
    pub repo_image_path: String,
    #[serde(default)]
    pub repositories: Vec<SpaceRepoRef>,
}

#[derive(Debug, Deserialize)]
pub struct SpaceRepoRef {
    /// Repository config JSON (the `create` input), relative to the space config file.
    pub config: String,
    /// Output subfolder under the space output directory; defaults to the config file stem.
    #[serde(default)]
    pub folder: Option<String>,
    /// Display name in `repository_space.json`; defaults to the repository's `repoName`.
    #[serde(default)]
    pub name: Option<String>,
    /// Public URL of the repository; defaults to `<baseUrl>/<folder>/`.
    #[serde(default)]
    pub address: Option<String>,
    #[serde(default = "default_true")]
    pub required: bool,
}

fn default_true() -> bool {
    true
}

/// One repository of the space with its config and resolved mods loaded.
#[derive(Debug)]
pub struct SpaceRepo {
    pub folder: String,
    pub name: String,
    pub address: String,
    pub required: bool,
    pub config: RepoConfig,
    /// The repository config file, kept so its relative entries (`modLineFiles`)
    /// resolve from the same directory as under `create`.
    pub config_path: PathBuf,
    pub mods: Vec<ResolvedMod>,
}

#[derive(Debug)]
pub struct LoadedSpace {
    pub config: SpaceConfig,
    /// Directory of the space config file; image paths resolve from here.
    pub config_dir: PathBuf,
    pub repos: Vec<SpaceRepo>,
}

/// Read the space config and load every referenced repository config.
///
/// Repository config paths resolve from the space config's directory; their
/// `basePath` keeps the `create` semantics (relative to the working directory).
pub fn load_space_config(path: &Path) -> Result<LoadedSpace> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read space config: {}", path.display()))?;
    let config: SpaceConfig =
        serde_json::from_str(&content).context("Failed to parse space config JSON")?;

    if config.name.trim().is_empty() {
        bail!("Space config needs a non-empty name");
    }
    if config.repositories.is_empty() {
        bail!("Space config lists no repositories");
    }

    let config_dir = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    let base_url = normalize_url(&config.base_url);

    let mut repos = Vec::with_capacity(config.repositories.len());
    for repo_ref in &config.repositories {
        let repo_config_path = resolve_from(&config_dir, &repo_ref.config);
        let (repo_config, mods) = config::load_config(&repo_config_path)
            .with_context(|| format!("Repository config {}", repo_config_path.display()))?;
        for warning in mod_line::game_config_warnings(&repo_config, &mods) {
            log::warn!("{}: {}", repo_config_path.display(), warning);
        }

        let folder = match repo_ref.folder.as_deref().map(str::trim) {
            Some(folder) if !folder.is_empty() => folder.to_string(),
            _ => default_folder_name(&repo_config_path)?,
        };
        validate_folder_name(&folder)?;

        let name = repo_ref
            .name
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .unwrap_or(repo_config.repo_name.trim())
            .to_string();
        if name.is_empty() {
            bail!("Repository {} has neither a name nor a repoName", folder);
        }

        let address = match repo_ref.address.as_deref().and_then(normalize_url) {
            Some(address) => address,
            None => match &base_url {
                Some(base) => format!("{base}{folder}/"),
                None => bail!(
                    "Repository {} has no address and the space config has no baseUrl",
                    folder
                ),
            },
        };

        repos.push(SpaceRepo {
            folder,
            name,
            address,
            required: repo_ref.required,
            config: repo_config,
            config_path: repo_config_path,
            mods,
        });
    }

    ensure_unique(
        repos.iter().map(|r| r.folder.to_lowercase()),
        "output folder",
    )?;
    ensure_unique(
        repos.iter().map(|r| r.name.to_lowercase()),
        "repository name",
    )?;
    ensure_unique(repos.iter().map(|r| r.address.to_lowercase()), "address")?;

    Ok(LoadedSpace {
        config,
        config_dir,
        repos,
    })
}

fn resolve_from(dir: &Path, relative: &str) -> PathBuf {
    let path = Path::new(relative);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        dir.join(path)
    }
}

fn default_folder_name(config_path: &Path) -> Result<String> {
    config_path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .map(str::to_string)
        .filter(|stem| !stem.is_empty())
        .with_context(|| {
            format!(
                "Cannot derive an output folder from {}; set \"folder\" explicitly",
                config_path.display()
            )
        })
}

/// A repository folder is one path component under the space output.
pub fn validate_folder_name(folder: &str) -> Result<()> {
    let component_count = Path::new(folder).components().count();
    let single_normal = matches!(
        Path::new(folder).components().next(),
        Some(Component::Normal(_))
    );
    if folder.is_empty()
        || component_count != 1
        || !single_normal
        || folder.contains(['/', '\\'])
        || folder == SPACE_MANIFEST
    {
        bail!(
            "Invalid repository folder {:?}: use a single folder name without path separators",
            folder
        );
    }
    Ok(())
}

/// Trim and add the trailing slash Foxy expects on repository URLs; `None`
/// when the value is blank.
pub fn normalize_url(url: &str) -> Option<String> {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(if trimmed.ends_with('/') {
        trimmed.to_string()
    } else {
        format!("{trimmed}/")
    })
}

fn ensure_unique(values: impl Iterator<Item = String>, what: &str) -> Result<()> {
    let mut seen = BTreeSet::new();
    for value in values {
        if !seen.insert(value.clone()) {
            bail!("Duplicate {} in space config: {}", what, value);
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Shared mods
// ---------------------------------------------------------------------------

/// One published mod name and every `(repo index, mod index)` slot that lists it.
#[derive(Debug, PartialEq, Eq)]
pub struct SharedMod {
    pub name: String,
    pub uses: Vec<(usize, usize)>,
}

/// Group every mod of the space by published name, in first-seen order.
pub fn group_shared_mods(repos: &[SpaceRepo]) -> Vec<SharedMod> {
    group_by_name(repos.iter().enumerate().flat_map(|(r, repo)| {
        repo.mods
            .iter()
            .enumerate()
            .map(move |(m, item)| (r, m, item))
    }))
}

fn group_by_name<'a>(
    slots: impl Iterator<Item = (usize, usize, &'a ResolvedMod)>,
) -> Vec<SharedMod> {
    let mut order: Vec<String> = Vec::new();
    let mut groups: BTreeMap<String, Vec<(usize, usize)>> = BTreeMap::new();
    for (r, m, item) in slots {
        let uses = groups.entry(item.mod_name.clone()).or_insert_with(|| {
            order.push(item.mod_name.clone());
            Vec::new()
        });
        uses.push((r, m));
    }
    order
        .into_iter()
        .map(|name| {
            let uses = groups.remove(&name).unwrap_or_default();
            SharedMod { name, uses }
        })
        .collect()
}

fn shared_mod_mismatch(name: &str, first: &Path, other: &Path) -> anyhow::Error {
    anyhow::anyhow!(
        "Mod {} differs between repositories of the same space:\n  {}\n  {}\nRepositories in one space share a client folder, so a mod name must resolve to identical content everywhere. Point both repositories at the same source or rename one of them.",
        name,
        first.display(),
        other.display()
    )
}

/// Every use of a shared name must hash identically to its first use.
fn ensure_shared_mods_match<'a>(
    groups: &[SharedMod],
    slot: impl Fn(usize, usize) -> (&'a Checksums, &'a Path),
) -> Result<()> {
    for group in groups {
        let Some(&(first_r, first_m)) = group.uses.first() else {
            continue;
        };
        let (first_sum, first_source) = slot(first_r, first_m);
        for &(r, m) in &group.uses[1..] {
            let (sum, source) = slot(r, m);
            if sum != first_sum {
                return Err(shared_mod_mismatch(&group.name, first_source, source));
            }
        }
    }
    Ok(())
}

fn with_repo_flags(processed: &ProcessedMod, resolved: &ResolvedMod) -> ProcessedMod {
    ProcessedMod {
        is_required: resolved.is_required,
        enabled: resolved.enabled,
        client_side: resolved.client_side,
        ..processed.clone()
    }
}

fn mod_bytes(processed: &ProcessedMod) -> u64 {
    processed.files.iter().map(|f| f.length).sum()
}

fn identity(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// The mods a pool build copies: each shared name taken from its first use.
fn pool_mods(space: &LoadedSpace, groups: &[SharedMod]) -> Vec<ResolvedMod> {
    groups
        .iter()
        .map(|group| {
            let (r, m) = group.uses[0];
            space.repos[r].mods[m].clone()
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Symlinks
// ---------------------------------------------------------------------------

/// Path from the directory holding `link` to `target`, so the output tree can
/// be moved as a whole; the absolute target when the two share no root.
pub fn symlink_target(link: &Path, target: &Path) -> PathBuf {
    let Some(link_dir) = link.parent() else {
        return target.to_path_buf();
    };
    relative_path(&identity(link_dir), &identity(target))
        .unwrap_or_else(|| std::path::absolute(target).unwrap_or_else(|_| target.to_path_buf()))
}

/// Relative path from `from_dir` to `to`; `None` when they share no prefix
/// (different drives) or either path is not absolute.
pub fn relative_path(from_dir: &Path, to: &Path) -> Option<PathBuf> {
    if !from_dir.is_absolute() || !to.is_absolute() {
        return None;
    }
    let from: Vec<Component<'_>> = from_dir.components().collect();
    let to: Vec<Component<'_>> = to.components().collect();
    let common = from
        .iter()
        .zip(to.iter())
        .take_while(|(a, b)| a == b)
        .count();
    if common == 0 || !matches!(from[0], Component::Prefix(_) | Component::RootDir) {
        return None;
    }
    let mut result = PathBuf::new();
    for _ in common..from.len() {
        result.push("..");
    }
    for component in &to[common..] {
        result.push(component.as_os_str());
    }
    if result.as_os_str().is_empty() {
        result.push(".");
    }
    Some(result)
}

/// Point `link` at `target`, replacing an existing symlink but never a real
/// file or directory.
pub fn replace_with_symlink(link: &Path, target: &Path) -> Result<()> {
    match std::fs::symlink_metadata(link) {
        Ok(meta) if meta.file_type().is_symlink() => remove_symlink(link)
            .with_context(|| format!("Failed to replace symlink {}", link.display()))?,
        Ok(_) => bail!(
            "{} already exists and is not a symlink; remove it before switching layouts, or keep --layout copy",
            link.display()
        ),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => {
            return Err(err).with_context(|| format!("Failed to inspect {}", link.display()));
        }
    }
    create_dir_symlink(target, link).with_context(|| {
        format!(
            "Failed to create symlink {} -> {}{}",
            link.display(),
            target.display(),
            symlink_hint()
        )
    })
}

#[cfg(windows)]
fn create_dir_symlink(target: &Path, link: &Path) -> std::io::Result<()> {
    std::os::windows::fs::symlink_dir(target, link)
}

#[cfg(not(windows))]
fn create_dir_symlink(target: &Path, link: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

#[cfg(windows)]
fn remove_symlink(link: &Path) -> std::io::Result<()> {
    std::fs::remove_dir(link).or_else(|_| std::fs::remove_file(link))
}

#[cfg(not(windows))]
fn remove_symlink(link: &Path) -> std::io::Result<()> {
    std::fs::remove_file(link)
}

fn symlink_hint() -> &'static str {
    if cfg!(windows) {
        " (creating symlinks on Windows needs Developer Mode or an elevated shell; --layout copy needs neither)"
    } else {
        ""
    }
}

// ---------------------------------------------------------------------------
// Output manifest
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct RepositorySpaceJson<'a> {
    name: &'a str,
    image: &'a str,
    #[serde(rename = "imageChecksum")]
    image_checksum: String,
    icon: &'a str,
    #[serde(rename = "iconChecksum")]
    icon_checksum: String,
    #[serde(rename = "appUpdateUrl", skip_serializing_if = "Option::is_none")]
    app_update_url: Option<&'a str>,
    entries: Vec<RepositorySpaceEntryJson<'a>>,
}

/// Entry shape read by the desktop app; `Requiered` is the legacy key it
/// expects.
#[derive(Serialize)]
struct RepositorySpaceEntryJson<'a> {
    #[serde(rename = "Name")]
    name: &'a str,
    #[serde(rename = "Address")]
    address: &'a str,
    #[serde(rename = "Requiered")]
    required: bool,
}

fn write_space_manifest(
    space: &LoadedSpace,
    output_dir: &Path,
    app_update_url: Option<&str>,
) -> Result<PathBuf> {
    let image_checksum =
        srf::copy_and_hash_image(&space.config.repo_image_path, &space.config_dir, output_dir)?;
    let icon_checksum =
        srf::copy_and_hash_image(&space.config.icon_image_path, &space.config_dir, output_dir)?;

    let manifest = RepositorySpaceJson {
        name: space.config.name.trim(),
        image: &space.config.repo_image_path,
        image_checksum,
        icon: &space.config.icon_image_path,
        icon_checksum,
        app_update_url,
        entries: space
            .repos
            .iter()
            .map(|repo| RepositorySpaceEntryJson {
                name: &repo.name,
                address: &repo.address,
                required: repo.required,
            })
            .collect(),
    };

    let json = serde_json::to_string_pretty(&manifest)
        .with_context(|| format!("Failed to serialize {SPACE_MANIFEST}"))?;
    let path = output_dir.join(SPACE_MANIFEST);
    std::fs::write(&path, json).with_context(|| format!("Failed to write {}", path.display()))?;
    Ok(path)
}

// ---------------------------------------------------------------------------
// Layouts
// ---------------------------------------------------------------------------

struct LayoutResult {
    /// Per repository, its mods in config order carrying that repository's flags.
    repo_mods: Vec<Vec<ProcessedMod>>,
    /// Generated key files to feed `--collect-keys`.
    key_sources: Vec<PathBuf>,
    copied_bytes: u64,
}

struct LayoutContext<'a> {
    space: &'a LoadedSpace,
    output_dir: &'a Path,
    groups: &'a [SharedMod],
    progress: &'a ProgressBar,
    mode: GenerationMode,
    prune_unused_optionals: bool,
    incremental: bool,
}

impl LayoutContext<'_> {
    fn repo_dir(&self, repo_index: usize) -> PathBuf {
        self.output_dir.join(&self.space.repos[repo_index].folder)
    }

    fn slot(&self, repo_index: usize, mod_index: usize) -> &ResolvedMod {
        &self.space.repos[repo_index].mods[mod_index]
    }

    /// Fill each repository's mod list from a per-group processed mod.
    fn assign_from_groups(
        &self,
        per_group: impl Fn(usize, usize, usize) -> ProcessedMod,
    ) -> Vec<Vec<ProcessedMod>> {
        let mut slots: Vec<Vec<Option<ProcessedMod>>> = self
            .space
            .repos
            .iter()
            .map(|repo| vec![None; repo.mods.len()])
            .collect();
        for (g, group) in self.groups.iter().enumerate() {
            for &(r, m) in &group.uses {
                slots[r][m] = Some(per_group(g, r, m));
            }
        }
        slots
            .into_iter()
            .map(|repo| repo.into_iter().flatten().collect())
            .collect()
    }
}

fn build_copy(ctx: &LayoutContext<'_>) -> Result<LayoutResult> {
    let mut repo_mods = Vec::with_capacity(ctx.space.repos.len());
    let mut key_sources = Vec::new();
    let mut copied_bytes = 0;

    for (r, repo) in ctx.space.repos.iter().enumerate() {
        let dir = ctx.repo_dir(r);
        println!(
            "[{}/{}] {} -> {}",
            r + 1,
            ctx.space.repos.len(),
            repo.name,
            dir.display()
        );
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("Failed to create {}", dir.display()))?;
        if ctx.prune_unused_optionals {
            published::remove_published_optionals(&dir, &repo.mods)?;
        }
        let processed = hash::process_mods(
            &repo.mods,
            Some(&dir),
            ctx.progress,
            ctx.mode,
            ctx.prune_unused_optionals,
            ctx.incremental,
        )?;
        artifacts::write_mod_manifests(&processed, &dir, ctx.mode)?;
        key_sources.extend(keys::generated_key_paths(&dir, &processed));
        copied_bytes += processed.iter().map(mod_bytes).sum::<u64>();
        repo_mods.push(processed);
    }

    ensure_shared_mods_match(ctx.groups, |r, m| {
        (
            &repo_mods[r][m].checksums,
            ctx.slot(r, m).source_path.as_path(),
        )
    })?;

    Ok(LayoutResult {
        repo_mods,
        key_sources,
        copied_bytes,
    })
}

fn build_pool(ctx: &LayoutContext<'_>, pool_dir: &Path) -> Result<LayoutResult> {
    std::fs::create_dir_all(pool_dir)
        .with_context(|| format!("Failed to create pool dir {}", pool_dir.display()))?;

    let unique = pool_mods(ctx.space, ctx.groups);
    println!(
        "Copying {} distinct mods into pool {}",
        unique.len(),
        pool_dir.display()
    );
    if ctx.prune_unused_optionals {
        published::remove_published_optionals(pool_dir, &unique)?;
    }
    let pooled = hash::process_mods(
        &unique,
        Some(pool_dir),
        ctx.progress,
        ctx.mode,
        ctx.prune_unused_optionals,
        ctx.incremental,
    )?;
    for item in &unique {
        std::fs::create_dir_all(pool_dir.join(&item.mod_name))
            .with_context(|| format!("Failed to create pool folder for {}", item.mod_name))?;
    }

    // A second repository may list the same mod from its own source folder:
    // hash that in place and refuse unless it matches what went into the pool.
    let mut extra: Vec<(usize, ResolvedMod)> = Vec::new();
    for (g, group) in ctx.groups.iter().enumerate() {
        let first = identity(&unique[g].source_path);
        for &(r, m) in &group.uses[1..] {
            let candidate = ctx.slot(r, m);
            if identity(&candidate.source_path) != first {
                extra.push((g, candidate.clone()));
            }
        }
    }
    if !extra.is_empty() {
        println!(
            "Verifying {} shared mods that come from a different source folder...",
            extra.len()
        );
        let candidates: Vec<ResolvedMod> = extra.iter().map(|(_, m)| m.clone()).collect();
        let hashed = hash::process_mods(
            &candidates,
            None,
            ctx.progress,
            ctx.mode,
            ctx.prune_unused_optionals,
            false,
        )?;
        for ((g, candidate), processed) in extra.iter().zip(hashed) {
            if processed.checksums != pooled[*g].checksums {
                return Err(shared_mod_mismatch(
                    &candidate.mod_name,
                    &unique[*g].source_path,
                    &candidate.source_path,
                ));
            }
        }
    }

    artifacts::write_mod_manifests(&pooled, pool_dir, ctx.mode)?;

    for (r, _) in ctx.space.repos.iter().enumerate() {
        let dir = ctx.repo_dir(r);
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("Failed to create {}", dir.display()))?;
    }
    for group in ctx.groups {
        let target = pool_dir.join(&group.name);
        for &(r, _) in &group.uses {
            let link = ctx.repo_dir(r).join(&group.name);
            replace_with_symlink(&link, &symlink_target(&link, &target))?;
        }
    }

    let repo_mods = ctx.assign_from_groups(|g, r, m| with_repo_flags(&pooled[g], ctx.slot(r, m)));
    let copied_bytes = pooled.iter().map(mod_bytes).sum();

    Ok(LayoutResult {
        repo_mods,
        key_sources: keys::generated_key_paths(pool_dir, &pooled),
        copied_bytes,
    })
}

fn build_link(ctx: &LayoutContext<'_>) -> Result<LayoutResult> {
    // Hash each distinct source folder once, even when several repositories
    // link to it.
    let mut distinct: Vec<ResolvedMod> = Vec::new();
    let mut distinct_ids: Vec<(String, PathBuf)> = Vec::new();
    let mut slot_to_distinct: BTreeMap<(usize, usize), usize> = BTreeMap::new();
    for group in ctx.groups {
        for &(r, m) in &group.uses {
            let candidate = ctx.slot(r, m);
            let id = (candidate.mod_name.clone(), identity(&candidate.source_path));
            let index = match distinct_ids.iter().position(|known| *known == id) {
                Some(index) => index,
                None => {
                    distinct_ids.push(id);
                    distinct.push(candidate.clone());
                    distinct.len() - 1
                }
            };
            slot_to_distinct.insert((r, m), index);
        }
    }

    println!("Hashing {} mod folders in place...", distinct.len());
    let hashed = hash::process_mods(&distinct, None, ctx.progress, ctx.mode, false, false)?;

    ensure_shared_mods_match(ctx.groups, |r, m| {
        let index = slot_to_distinct[&(r, m)];
        (
            &hashed[index].checksums,
            distinct[index].source_path.as_path(),
        )
    })?;

    let mut key_sources = Vec::new();
    let mut repo_mods = Vec::with_capacity(ctx.space.repos.len());
    for (r, repo) in ctx.space.repos.iter().enumerate() {
        let dir = ctx.repo_dir(r);
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("Failed to create {}", dir.display()))?;
        let mut processed = Vec::with_capacity(repo.mods.len());
        for (m, resolved) in repo.mods.iter().enumerate() {
            let link = dir.join(&resolved.mod_name);
            let target = std::path::absolute(&resolved.source_path)
                .unwrap_or_else(|_| resolved.source_path.clone());
            replace_with_symlink(&link, &target)?;
            processed.push(with_repo_flags(
                &hashed[slot_to_distinct[&(r, m)]],
                resolved,
            ));
        }
        artifacts::write_mod_manifests(&processed, &dir, ctx.mode)?;
        key_sources.extend(keys::generated_key_paths(&dir, &processed));
        repo_mods.push(processed);
    }

    Ok(LayoutResult {
        repo_mods,
        key_sources,
        copied_bytes: 0,
    })
}

// ---------------------------------------------------------------------------
// Command
// ---------------------------------------------------------------------------

pub struct CreateSpaceOptions<'a> {
    pub layout: SpaceLayout,
    pub pool_dir: Option<PathBuf>,
    pub yes: bool,
    pub clean: bool,
    pub dry_run: bool,
    pub atomic: bool,
    pub prune_unused_optionals: bool,
    pub incremental: bool,
    pub only: Vec<String>,
    pub app_update_url: Option<&'a str>,
    pub threads: usize,
    pub mode: GenerationMode,
    pub no_progress: bool,
    pub mod_line: mod_line::ModLineOptions<'a>,
    pub keys: KeyCollectionRequest,
    /// Write `<repo>/keys` with each repository's own keys next to its manifests.
    pub per_repo_keys: bool,
}

pub fn cmd_create_space(
    config_path: &Path,
    output_dir: &Path,
    options: CreateSpaceOptions<'_>,
) -> Result<Vec<mod_line_files::PendingUpdate>> {
    let CreateSpaceOptions {
        layout,
        pool_dir,
        yes,
        clean,
        dry_run,
        atomic,
        prune_unused_optionals,
        incremental,
        only,
        app_update_url,
        threads,
        mode,
        no_progress,
        mod_line: mod_line_options,
        keys: key_collection,
        per_repo_keys,
    } = options;
    let started = Instant::now();

    if layout == SpaceLayout::Link && !yes && !dry_run {
        bail!(
            "--layout link publishes the source mod folders in place: their manifests are written next to the mod files and any later edit there silently breaks the published checksums. Re-run with --yes to accept that, or use --layout pool, which copies each mod once and stays safe to edit."
        );
    }
    if prune_unused_optionals && layout == SpaceLayout::Link {
        bail!("--prune-unused-optionals cannot be used with --layout link");
    }
    if clean && layout != SpaceLayout::Pool {
        bail!("--clean requires --layout pool");
    }
    if clean && !dry_run && !yes {
        bail!("--clean removes generated files; preview with --dry-run or re-run with --yes");
    }
    if !only.is_empty() && clean {
        bail!(
            "--only cannot be combined with --clean because other repositories still use the pool"
        );
    }
    if !only.is_empty() && key_collection.enabled {
        bail!("--only cannot rebuild the combined keys folder; use --per-repo-keys");
    }

    let mode_label = artifacts::mode_label(mode);
    println!("Mode: {}", mode_label);
    println!("Layout: {}", layout.as_str());

    println!("Loading space config from: {}", config_path.display());
    let mut space = load_space_config(config_path)?;
    let manifest_space = if only.is_empty() {
        None
    } else {
        let full = load_space_config(config_path)?;
        let wanted: BTreeSet<String> = only.iter().map(|name| name.to_lowercase()).collect();
        for name in &wanted {
            if !full
                .repos
                .iter()
                .any(|repo| repo.folder.to_lowercase() == *name)
            {
                bail!("Unknown repository folder for --only: {name}");
            }
        }
        for group in group_shared_mods(&full.repos) {
            let selected = group
                .uses
                .iter()
                .any(|(r, _)| wanted.contains(&full.repos[*r].folder.to_lowercase()));
            let unselected = group
                .uses
                .iter()
                .any(|(r, _)| !wanted.contains(&full.repos[*r].folder.to_lowercase()));
            if selected && unselected {
                bail!(
                    "--only cannot rebuild shared mod {} without updating all repositories that use it",
                    group.name
                );
            }
        }
        for repo in &full.repos {
            if !wanted.contains(&repo.folder.to_lowercase())
                && !output_dir.join(&repo.folder).join("repo.json").is_file()
            {
                bail!(
                    "--only needs existing output for repository {}",
                    repo.folder
                );
            }
        }
        space
            .repos
            .retain(|repo| wanted.contains(&repo.folder.to_lowercase()));
        Some(full)
    };
    let groups = group_shared_mods(&space.repos);
    let shared_count = groups.iter().filter(|g| g.uses.len() > 1).count();

    println!(
        "Repository space: {} ({} repositories, {} distinct mods, {} shared)",
        space.config.name.trim(),
        space.repos.len(),
        groups.len(),
        shared_count
    );
    for repo in &space.repos {
        println!(
            "  {} -> {}/ ({}, {} mods) {}",
            repo.name,
            repo.folder,
            if repo.required {
                "required"
            } else {
                "optional"
            },
            repo.mods.len(),
            repo.address
        );
    }

    let launch_files: Vec<Vec<PathBuf>> = space
        .repos
        .iter()
        .map(|repo| mod_line_files::resolve(&repo.config, &repo.config_path))
        .collect();
    for (repo, files) in space.repos.iter().zip(&launch_files) {
        mod_line_files::check(files, &mod_line::launch_flags(repo.config.game))
            .with_context(|| format!("Repository {}", repo.folder))?;
    }
    mod_line_files::ensure_distinct(
        space
            .repos
            .iter()
            .map(|repo| repo.folder.as_str())
            .zip(launch_files.iter().map(Vec::as_slice)),
    )?;

    if dry_run {
        let pool_dir = pool_dir.unwrap_or_else(|| output_dir.join(DEFAULT_POOL_DIR));
        let mut plan = planner::create_space(
            &space,
            output_dir,
            layout,
            &pool_dir,
            mode,
            prune_unused_optionals,
            incremental,
        )?;
        if atomic && output_dir.exists() {
            plan.add("replace-output", output_dir, 0);
        }
        if clean {
            let cleanup = cleanup::plan_cleanup(output_dir, &pool_dir, &groups)?;
            for link in cleanup.links() {
                plan.add("remove-link", link, 0);
            }
            for dir in cleanup.mods() {
                plan.add("remove", dir, 0);
            }
        }
        if key_collection.enabled {
            let dest = key_collection
                .dest
                .clone()
                .unwrap_or_else(|| output_dir.join(DEFAULT_KEYS_DIR));
            let mods: Vec<_> = space
                .repos
                .iter()
                .flat_map(|repo| repo.mods.clone())
                .collect();
            plan.add_keys(
                &mods,
                &dest,
                &key_collection.additional_sources,
                prune_unused_optionals,
            )?;
        }
        if per_repo_keys {
            for repo in &space.repos {
                plan.add_keys(
                    &repo.mods,
                    &output_dir.join(&repo.folder).join(DEFAULT_KEYS_DIR),
                    &key_collection.additional_sources,
                    prune_unused_optionals,
                )?;
            }
        }
        for path in launch_files.iter().flatten() {
            plan.add("update-mod-line", path, 0);
        }
        plan.show();
        return Ok(Vec::new());
    }

    crate::configure_thread_pool(threads)?;

    let pool_dir = pool_dir.unwrap_or_else(|| output_dir.join(DEFAULT_POOL_DIR));
    if prune_unused_optionals {
        let mut found = Vec::new();
        if layout == SpaceLayout::Pool {
            found = published::published_optionals(&pool_dir, &pool_mods(&space, &groups))?;
        } else {
            for repo in &space.repos {
                found.extend(published::published_optionals(
                    &output_dir.join(&repo.folder),
                    &repo.mods,
                )?);
            }
        }
        published::confirm_prune(&found, yes)?;
    }

    std::fs::create_dir_all(output_dir)
        .with_context(|| format!("Failed to create output dir: {}", output_dir.display()))?;

    let keys_dir = key_collection
        .dest
        .clone()
        .unwrap_or_else(|| output_dir.join(DEFAULT_KEYS_DIR));
    let mut reserved: Vec<&Path> = Vec::new();
    if layout == SpaceLayout::Pool {
        reserved.push(&pool_dir);
    }
    if key_collection.enabled {
        reserved.push(&keys_dir);
    }
    ensure_folders_free(&space, output_dir, &reserved)?;
    if per_repo_keys {
        ensure_repo_keys_dir_free(
            space
                .repos
                .iter()
                .map(|r| (r.folder.as_str(), r.mods.as_slice())),
        )?;
    }
    if clean {
        cleanup::plan_cleanup(output_dir, &pool_dir, &groups)?;
    }

    let progress = crate::progress_bar(no_progress);
    println!("Processing files with {} threads...", threads);
    let ctx = LayoutContext {
        space: &space,
        output_dir,
        groups: &groups,
        progress: &progress,
        mode,
        prune_unused_optionals,
        incremental,
    };
    let built = match layout {
        SpaceLayout::Copy => build_copy(&ctx),
        SpaceLayout::Pool => build_pool(&ctx, &pool_dir),
        SpaceLayout::Link => build_link(&ctx),
    };
    progress.finish_and_clear();
    let built = built?;

    let space_app_update_url = app_update_url
        .or(space.config.app_update_url.as_deref())
        .map(str::trim)
        .filter(|url| !url.is_empty());

    println!("Writing repository manifests...");
    let mut checksums = Vec::with_capacity(space.repos.len());
    for (r, repo) in space.repos.iter().enumerate() {
        let dir = ctx.repo_dir(r);
        let repo_app_update_url = app_update_url
            .or(repo.config.app_update_url.as_deref())
            .or(space_app_update_url);
        let checksum = artifacts::write_repo_manifests(
            &repo.config,
            &built.repo_mods[r],
            &dir,
            mode,
            repo_app_update_url,
        )?;
        checksums.push(checksum);
    }

    println!("Writing {}...", SPACE_MANIFEST);
    let manifest_path = write_space_manifest(
        manifest_space.as_ref().unwrap_or(&space),
        output_dir,
        space_app_update_url,
    )?;

    let key_report = if key_collection.enabled {
        println!("Collecting keys into: {}", keys_dir.display());
        let report = keys::collect_key_paths(
            built.key_sources,
            &keys::KeyCollectionOptions {
                dest: &keys_dir,
                additional_sources: &key_collection.additional_sources,
            },
        )?;
        for name in &report.conflicts {
            log::warn!(
                "Multiple different keys named {}; kept the first one found",
                name
            );
        }
        Some(report)
    } else {
        None
    };

    let repo_key_reports = if per_repo_keys {
        let mut reports = Vec::with_capacity(space.repos.len());
        for (r, repo) in space.repos.iter().enumerate() {
            let dir = ctx.repo_dir(r);
            let dest = dir.join(DEFAULT_KEYS_DIR);
            println!("Collecting {} keys into: {}", repo.folder, dest.display());
            let report = keys::collect_key_paths(
                keys::generated_key_paths(&dir, &built.repo_mods[r]),
                &keys::KeyCollectionOptions {
                    dest: &dest,
                    additional_sources: &key_collection.additional_sources,
                },
            )?;
            for name in &report.conflicts {
                log::warn!(
                    "{}: multiple different keys named {}; kept the first one found",
                    repo.folder,
                    name
                );
            }
            reports.push((dest, report));
        }
        reports
    } else {
        Vec::new()
    };

    let launch_params: Vec<Vec<mod_line::LaunchParam>> = space
        .repos
        .iter()
        .enumerate()
        .map(|(r, repo)| {
            mod_line::build_launch_params(
                &repo.config,
                &built.repo_mods[r],
                &repo.mods,
                mod_line_options,
            )
        })
        .collect();
    let server_lines: Vec<String> = launch_params
        .iter()
        .map(|params| mod_line::render_launch_params(params))
        .collect();
    for (r, line) in server_lines.iter().enumerate() {
        published::write_server_mod_line(&ctx.repo_dir(r), line)?;
    }

    if layout == SpaceLayout::Pool {
        if clean {
            let report = cleanup::plan_cleanup(output_dir, &pool_dir, &groups)?;
            report.execute()?;
            report.print(false);
        }
        cleanup::write_inventory(
            output_dir,
            &pool_dir,
            manifest_space.as_ref().unwrap_or(&space),
            &groups,
            clean,
        )?;
    }

    let content_bytes: u64 = built.repo_mods.iter().flatten().map(mod_bytes).sum();
    let duplicated_bytes: u64 = groups
        .iter()
        .filter(|g| g.uses.len() > 1)
        .map(|g| {
            let (r, m) = g.uses[0];
            mod_bytes(&built.repo_mods[r][m]) * (g.uses.len() as u64 - 1)
        })
        .sum();
    let elapsed = started.elapsed();

    println!();
    println!("Done!");
    println!("  Mode:       {}", mode_label);
    println!("  Layout:     {}", layout.as_str());
    println!("  Repos:      {}", space.repos.len());
    println!(
        "  Mods:       {} distinct ({} shared between repositories)",
        groups.len(),
        shared_count
    );
    println!("  Content:    {:.2} MB", mb(content_bytes));
    println!("  Copied:     {:.2} MB", mb(built.copied_bytes));
    println!("  Time:       {:.2}s", elapsed.as_secs_f64());
    println!("  Output:     {}", output_dir.display());
    println!("  Manifest:   {}", manifest_path.display());
    if layout == SpaceLayout::Pool {
        println!("  Pool:       {}", pool_dir.display());
    }
    for line in artifacts::artifact_lines(mode) {
        println!("  Artifacts:  {}", line);
    }
    for (repo, checksum) in space.repos.iter().zip(&checksums) {
        println!("  {}: {}", repo.folder, checksum);
    }
    if let Some(report) = &key_report {
        println!("  Keys:       {} in {}", report.copied, keys_dir.display());
        if report.duplicates > 0 {
            println!("              {} duplicate keys skipped", report.duplicates);
        }
        if !report.conflicts.is_empty() {
            println!(
                "              {} conflicting key names kept at first match: {}",
                report.conflicts.len(),
                report.conflicts.join(", ")
            );
        }
    }
    for (dest, report) in &repo_key_reports {
        println!("  Keys:       {} in {}", report.copied, dest.display());
        if report.duplicates > 0 {
            println!("              {} duplicate keys skipped", report.duplicates);
        }
        if !report.conflicts.is_empty() {
            println!(
                "              {} conflicting key names kept at first match: {}",
                report.conflicts.len(),
                report.conflicts.join(", ")
            );
        }
    }

    match layout {
        SpaceLayout::Copy if shared_count > 0 => {
            println!();
            println!(
                "Hint: {} shared mods take {:.2} MB of extra space. --layout pool copies each of them once and symlinks the repositories to it.",
                shared_count,
                mb(duplicated_bytes)
            );
        }
        SpaceLayout::Link => {
            println!();
            println!(
                "Warning: the source mod folders are now published in place. Re-run create-space after any change to them; --layout pool avoids this."
            );
        }
        _ => {}
    }

    println!();
    println!("Server mod lines:");
    for (repo, line) in space.repos.iter().zip(&server_lines) {
        println!("{}:", repo.folder);
        println!("{line}");
    }

    let lines: Vec<_> = space
        .repos
        .iter()
        .zip(&server_lines)
        .map(|(repo, line)| serde_json::json!({"folder": repo.folder, "line": line}))
        .collect();
    let warnings: Vec<_> = space
        .repos
        .iter()
        .flat_map(|repo| {
            mod_line::game_config_warnings(&repo.config, &repo.mods)
                .into_iter()
                .map(|warning| format!("{}: {warning}", repo.folder))
        })
        .collect();
    crate::output::set_details(serde_json::json!({
        "output": output_dir,
        "layout": layout.as_str(),
        "repositories": space.repos.len(),
        "distinctMods": groups.len(),
        "serverLines": lines,
        "warnings": warnings,
        "keyConflicts": key_report.as_ref().map(|report| report.conflicts.clone()).unwrap_or_default(),
        "perRepoKeyConflicts": repo_key_reports.iter().map(|(dest, report)| serde_json::json!({"path": dest, "names": report.conflicts})).collect::<Vec<_>>(),
    }));

    Ok(launch_files
        .into_iter()
        .zip(launch_params)
        .map(|(files, params)| mod_line_files::PendingUpdate { files, params })
        .collect())
}

fn mb(bytes: u64) -> f64 {
    bytes as f64 / 1024.0 / 1024.0
}

/// A repository folder may not collide with the pool or keys folder when
/// those live directly under the space output.
fn ensure_folders_free(space: &LoadedSpace, output_dir: &Path, reserved: &[&Path]) -> Result<()> {
    let output_id = identity(output_dir);
    for dir in reserved {
        let parent_is_output = dir
            .parent()
            .map(|parent| identity(parent) == output_id)
            .unwrap_or(false);
        let Some(name) = dir.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if parent_is_output
            && let Some(repo) = space
                .repos
                .iter()
                .find(|repo| repo.folder.eq_ignore_ascii_case(name))
        {
            bail!(
                "Repository folder {} collides with {}; rename the repository folder or move that directory",
                repo.folder,
                dir.display()
            );
        }
    }
    Ok(())
}

/// `--per-repo-keys` writes `<repo>/keys`, which must not be a mod folder of that repository.
fn ensure_repo_keys_dir_free<'a>(
    repos: impl Iterator<Item = (&'a str, &'a [ResolvedMod])>,
) -> Result<()> {
    for (folder, mods) in repos {
        if let Some(m) = mods
            .iter()
            .find(|m| m.mod_name.eq_ignore_ascii_case(DEFAULT_KEYS_DIR))
        {
            bail!(
                "Mod {} in repository {} collides with the per-repository keys folder; rename the mod or drop --per-repo-keys",
                m.mod_name,
                folder
            );
        }
    }
    Ok(())
}

pub fn cmd_new_space(output: &Path) -> Result<()> {
    if output.exists() {
        bail!(
            "File already exists: {}. Remove it first or choose a different path.",
            output.display()
        );
    }
    let template = serde_json::json!({
        "name": "My Repository Space",
        "baseUrl": "https://example.com/repos/",
        "appUpdateUrl": "",
        "iconImagePath": "icon.png",
        "repoImagePath": "space.png",
        "repositories": [
            { "config": "modern/config.json", "folder": "modern", "required": true },
            { "config": "ww2/config.json", "folder": "ww2", "required": false }
        ]
    });
    let json = serde_json::to_string_pretty(&template).context("Failed to serialize template")?;
    std::fs::write(output, json)
        .with_context(|| format!("Failed to write {}", output.display()))?;
    println!("Space config template written to: {}", output.display());
    println!("Each entry points at a repository config made with `new`. Then run:");
    println!(
        "  foxy-server-backend-cli create-space {} <output-dir> --layout pool",
        output.display()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resolved(name: &str, source: &str) -> ResolvedMod {
        ResolvedMod {
            mod_name: name.to_string(),
            source_path: PathBuf::from(source),
            is_required: true,
            enabled: true,
            client_side: false,
        }
    }

    #[test]
    fn normalize_url_adds_trailing_slash_and_drops_blank() {
        assert_eq!(
            normalize_url(" https://a.example/x "),
            Some("https://a.example/x/".to_string())
        );
        assert_eq!(
            normalize_url("https://a.example/x/"),
            Some("https://a.example/x/".to_string())
        );
        assert_eq!(normalize_url("   "), None);
    }

    #[test]
    fn per_repo_keys_dir_refuses_a_mod_named_keys() {
        let fine = [resolved("@ace", "src/@ace")];
        let clash = [resolved("@cba", "src/@cba"), resolved("Keys", "src/Keys")];
        assert!(ensure_repo_keys_dir_free([("modern", &fine[..])].into_iter()).is_ok());
        let err =
            ensure_repo_keys_dir_free([("modern", &fine[..]), ("ww2", &clash[..])].into_iter())
                .unwrap_err()
                .to_string();
        assert!(err.contains("Keys") && err.contains("ww2"), "{err}");
    }

    #[test]
    fn folder_names_are_single_components() {
        assert!(validate_folder_name("modern").is_ok());
        assert!(validate_folder_name("WW2 Pack").is_ok());
        assert!(validate_folder_name("").is_err());
        assert!(validate_folder_name(".").is_err());
        assert!(validate_folder_name("..").is_err());
        assert!(validate_folder_name("a/b").is_err());
        assert!(validate_folder_name("a\\b").is_err());
        assert!(validate_folder_name(SPACE_MANIFEST).is_err());
    }

    #[test]
    fn shared_mods_group_by_name_in_first_seen_order() {
        let slots = [
            (0, 0, resolved("@cba", "/mods/a/@cba")),
            (0, 1, resolved("@ace", "/mods/a/@ace")),
            (1, 0, resolved("@cba", "/mods/b/@cba")),
            (1, 1, resolved("@ww2", "/mods/b/@ww2")),
        ];
        let groups = group_by_name(slots.iter().map(|(r, m, item)| (*r, *m, item)));
        assert_eq!(
            groups,
            vec![
                SharedMod {
                    name: "@cba".to_string(),
                    uses: vec![(0, 0), (1, 0)]
                },
                SharedMod {
                    name: "@ace".to_string(),
                    uses: vec![(0, 1)]
                },
                SharedMod {
                    name: "@ww2".to_string(),
                    uses: vec![(1, 1)]
                },
            ]
        );
    }

    #[test]
    fn mismatching_shared_mod_is_rejected() {
        let groups = vec![SharedMod {
            name: "@cba".to_string(),
            uses: vec![(0, 0), (1, 0)],
        }];
        let sums = [Checksums::Blake3("A".into()), Checksums::Blake3("B".into())];
        let sources = [PathBuf::from("/a/@cba"), PathBuf::from("/b/@cba")];
        let err =
            ensure_shared_mods_match(&groups, |r, _| (&sums[r], sources[r].as_path())).unwrap_err();
        assert!(err.to_string().contains("@cba"));

        let same = [Checksums::Blake3("A".into()), Checksums::Blake3("A".into())];
        assert!(ensure_shared_mods_match(&groups, |r, _| (&same[r], sources[r].as_path())).is_ok());
    }

    #[cfg(windows)]
    #[test]
    fn relative_path_walks_up_to_common_root() {
        let from = Path::new(r"C:\srv\space\modern");
        let to = Path::new(r"C:\srv\space\pool\@cba");
        assert_eq!(
            relative_path(from, to),
            Some(PathBuf::from(r"..\pool\@cba"))
        );
        assert_eq!(relative_path(from, Path::new(r"D:\other\@cba")), None);
        assert_eq!(relative_path(from, from), Some(PathBuf::from(".")));
    }

    #[cfg(not(windows))]
    #[test]
    fn relative_path_walks_up_to_common_root() {
        let from = Path::new("/srv/space/modern");
        let to = Path::new("/srv/space/pool/@cba");
        assert_eq!(relative_path(from, to), Some(PathBuf::from("../pool/@cba")));
        assert_eq!(relative_path(from, from), Some(PathBuf::from(".")));
        assert_eq!(relative_path(Path::new("relative"), to), None);
    }

    #[test]
    fn replace_with_symlink_refuses_real_directories() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("@real");
        let target = dir.path().join("@target");
        std::fs::create_dir_all(&real).unwrap();
        std::fs::create_dir_all(&target).unwrap();
        let err = replace_with_symlink(&real, &target).unwrap_err();
        assert!(err.to_string().contains("not a symlink"));
    }

    #[test]
    fn space_manifest_uses_legacy_entry_keys() {
        let entries = vec![RepositorySpaceEntryJson {
            name: "Modern",
            address: "https://example.com/repos/modern/",
            required: true,
        }];
        let manifest = RepositorySpaceJson {
            name: "Space",
            image: "",
            image_checksum: String::new(),
            icon: "",
            icon_checksum: String::new(),
            app_update_url: None,
            entries,
        };
        let json = serde_json::to_value(&manifest).unwrap();
        assert_eq!(json["entries"][0]["Name"], "Modern");
        assert_eq!(
            json["entries"][0]["Address"],
            "https://example.com/repos/modern/"
        );
        assert_eq!(json["entries"][0]["Requiered"], true);
        assert!(json.get("appUpdateUrl").is_none());
    }

    fn write_repo_config(dir: &Path, name: &str, mods: &[&str]) -> PathBuf {
        let base = dir.join(name);
        for m in mods {
            let mod_dir = base.join(m).join("addons");
            std::fs::create_dir_all(&mod_dir).unwrap();
            std::fs::write(mod_dir.join("data.pbo"), format!("{m} payload")).unwrap();
        }
        let escaped = base.to_string_lossy().replace('\\', "\\\\");
        let required: Vec<String> = mods
            .iter()
            .map(|m| format!(r#"{{ "modName": "{m}", "enabled": true }}"#))
            .collect();
        let config = format!(
            r#"{{ "repoName": "{name}", "basePath": "{escaped}", "requiredMods": [{}] }}"#,
            required.join(",")
        );
        let path = dir.join(format!("{name}.json"));
        std::fs::write(&path, config).unwrap();
        path
    }

    #[test]
    fn load_space_config_derives_folders_names_and_addresses() {
        let dir = tempfile::tempdir().unwrap();
        write_repo_config(dir.path(), "modern", &["@cba"]);
        write_repo_config(dir.path(), "ww2", &["@cba", "@ifa3"]);
        let space_path = dir.path().join("space.json");
        std::fs::write(
            &space_path,
            r#"{
              "name": "Test Space",
              "baseUrl": "https://example.com/repos",
              "repositories": [
                { "config": "modern.json" },
                { "config": "ww2.json", "folder": "WW2", "name": "World War 2", "required": false, "address": "https://cdn.example.com/ww2" }
              ]
            }"#,
        )
        .unwrap();

        let space = load_space_config(&space_path).unwrap();
        assert_eq!(space.repos.len(), 2);
        assert_eq!(space.repos[0].folder, "modern");
        assert_eq!(space.repos[0].name, "modern");
        assert_eq!(space.repos[0].address, "https://example.com/repos/modern/");
        assert!(space.repos[0].required);
        assert_eq!(space.repos[1].folder, "WW2");
        assert_eq!(space.repos[1].name, "World War 2");
        assert_eq!(space.repos[1].address, "https://cdn.example.com/ww2/");
        assert!(!space.repos[1].required);

        let groups = group_shared_mods(&space.repos);
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].name, "@cba");
        assert_eq!(groups[0].uses, vec![(0, 0), (1, 0)]);
    }

    #[test]
    fn load_space_config_rejects_missing_base_url_and_duplicate_folders() {
        let dir = tempfile::tempdir().unwrap();
        write_repo_config(dir.path(), "modern", &["@cba"]);
        let space_path = dir.path().join("space.json");

        std::fs::write(
            &space_path,
            r#"{ "name": "S", "repositories": [ { "config": "modern.json" } ] }"#,
        )
        .unwrap();
        let err = load_space_config(&space_path).unwrap_err();
        assert!(err.to_string().contains("baseUrl"));

        std::fs::write(
            &space_path,
            r#"{ "name": "S", "baseUrl": "https://x/", "repositories": [
                { "config": "modern.json", "folder": "a" },
                { "config": "modern.json", "folder": "A", "name": "other" } ] }"#,
        )
        .unwrap();
        let err = load_space_config(&space_path).unwrap_err();
        assert!(err.to_string().contains("Duplicate output folder"));
    }
}
