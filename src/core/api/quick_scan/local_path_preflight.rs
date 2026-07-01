use crate::core::models::model_tree::Tree;
use crate::core::utils::format::sanitize_log_path_str;
use log::{info, warn};
use std::path::Path;

const SAMPLE_LIMIT: usize = 5;
const SUSPECT_MIN_FILES: usize = 20;
const SUSPECT_MISSING_FILE_RATIO_NUMERATOR: usize = 9;
const SUSPECT_MISSING_FILE_RATIO_DENOMINATOR: usize = 10;
const SUSPECT_MISSING_ADDON_RATIO_NUMERATOR: usize = 1;
const SUSPECT_MISSING_ADDON_RATIO_DENOMINATOR: usize = 2;

/// An addon counts as "content present but unresolved" (a layout/path mismatch
/// signal) when its folder holds at least this fraction of its expected files
/// on disk while *none* of those expected files resolve at their manifest
/// paths. Half is conservative: it tolerates older/partial on-disk versions
/// while still requiring the folder to clearly contain the addon's content.
const LAYOUT_MISMATCH_MIN_DISK_RATIO_NUMERATOR: usize = 1;
const LAYOUT_MISMATCH_MIN_DISK_RATIO_DENOMINATOR: usize = 2;
/// Upper bound on directory entries scanned per addon when probing for on-disk
/// content, so a pathological tree cannot stall the preflight.
const LAYOUT_MISMATCH_ENTRY_BUDGET: usize = 8192;
/// Cap on the number of problem addons captured for detailed diagnostics.
const DIAGNOSTIC_ADDON_LIMIT: usize = 12;
/// Cap on directory entries listed per folder when logging the on-disk layout.
const DISK_SNAPSHOT_ENTRY_LIMIT: usize = 16;
/// Upper bound on directory entries scanned when locating expected filenames
/// across the whole repo root (the shared-redirection probe). Larger than the
/// per-addon budget because it may traverse sibling repos in a shared space, but
/// still bounded so a huge tree cannot stall the suspect-path logging.
const ROOT_LOCATE_ENTRY_BUDGET: usize = 50_000;

/// Why an addon was flagged as a path problem worth detailed logging.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AddonPathProblem {
    /// The addon's configured folder does not exist on disk.
    MissingDir,
    /// The folder exists and holds files, but none of the expected files resolve
    /// at their declared paths - a layout/path-casing mismatch.
    LayoutMismatch,
    /// The folder exists but holds little/no matching content - a genuine
    /// missing download.
    FilesMissing,
}

/// Per-addon detail captured for problem addons so logs can pinpoint the exact
/// path and on-disk state when a redownload or path mismatch is flagged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AddonPathDiagnostic {
    pub(crate) name: String,
    pub(crate) configured_path: String,
    pub(crate) dir_exists: bool,
    pub(crate) expected_files: usize,
    pub(crate) resolved_files: usize,
    /// On-disk file count beneath the addon folder (capped at `expected_files`).
    pub(crate) on_disk_files: usize,
    pub(crate) problem: AddonPathProblem,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LocalPathAvailability {
    pub(crate) repo_root: String,
    pub(crate) root_exists: bool,
    pub(crate) expected_addons: usize,
    pub(crate) existing_addons: usize,
    pub(crate) missing_addon_dirs: usize,
    pub(crate) expected_files: usize,
    pub(crate) existing_files: usize,
    pub(crate) existing_files_with_expected_size: usize,
    pub(crate) missing_files: usize,
    /// Addons whose folder exists and holds files on disk, yet none of the
    /// addon's expected files resolve at their manifest-declared paths. This is
    /// the signature of a layout/path-casing mismatch: the content was
    /// downloaded but lives under unexpected names/subfolders, so flagging a
    /// (re)download would be wrong.
    pub(crate) addons_with_disk_content_unresolved: usize,
    pub(crate) had_prior_local_state: bool,
    pub(crate) sample_missing_addon_dirs: Vec<String>,
    pub(crate) sample_missing_files: Vec<String>,
    /// Sample of real on-disk file paths found inside addon folders whose
    /// expected files did not resolve - surfaced for diagnostics so the actual
    /// layout difference is visible without filesystem access.
    pub(crate) sample_unresolved_disk_paths: Vec<String>,
    /// Per-addon detail for addons that look problematic (missing folder, layout
    /// mismatch, or no matching content), capped at [`DIAGNOSTIC_ADDON_LIMIT`].
    pub(crate) addon_diagnostics: Vec<AddonPathDiagnostic>,
}

impl LocalPathAvailability {
    pub(crate) fn missing_file_percent(&self) -> usize {
        self.missing_files
            .saturating_mul(100)
            .checked_div(self.expected_files)
            .unwrap_or(0)
    }

    pub(crate) fn looks_like_empty_download_destination(&self) -> bool {
        self.root_exists
            && self.existing_addons == 0
            && self.existing_files == 0
            && self.missing_files == self.expected_files
            && self.addons_with_disk_content_unresolved == 0
    }

    /// `true` when on-disk addon content exists but does not line up with the
    /// manifest's expected file paths - a layout/path mismatch rather than a
    /// missing download.
    pub(crate) fn layout_mismatch_suspected(&self) -> bool {
        self.addons_with_disk_content_unresolved > 0
    }
}

pub(crate) fn summarize_local_path_availability(tree: &Tree) -> LocalPathAvailability {
    let repo_root = tree
        .repositories
        .first()
        .map(|repo| repo.local_path.clone())
        .unwrap_or_default();
    let root_exists = !repo_root.trim().is_empty() && Path::new(&repo_root).is_dir();

    // Prior-download evidence. A repo that was previously verified/downloaded carries
    // a non-empty local_checksum or local_content_hash on the repository row, or on any of its
    // addon/file rows. A never-downloaded repo (e.g. migrated metadata, or a fresh install into
    // a shared space populated by sibling repos) has none of these.
    let has_state = |checksum: &str, content_hash: &str| {
        !checksum.trim().is_empty() || !content_hash.trim().is_empty()
    };
    let had_prior_local_state = tree
        .repositories
        .iter()
        .any(|r| has_state(&r.local_checksum, &r.local_content_hash))
        || tree
            .mods
            .iter()
            .any(|m| has_state(&m.local_checksum, &m.local_content_hash))
        || tree
            .files
            .iter()
            .any(|f| has_state(&f.local_checksum, &f.local_content_hash));

    let mut expected_addons = 0usize;
    let mut existing_addons = 0usize;
    let mut missing_addon_dirs = 0usize;
    let mut expected_files = 0usize;
    let mut existing_files = 0usize;
    let mut existing_files_with_expected_size = 0usize;
    let mut missing_files = 0usize;
    let mut addons_with_disk_content_unresolved = 0usize;
    let mut sample_missing_addon_dirs = Vec::new();
    let mut sample_missing_files = Vec::new();
    let mut sample_unresolved_disk_paths = Vec::new();
    let mut addon_diagnostics: Vec<AddonPathDiagnostic> = Vec::new();

    for mod_node in &tree.mod_nodes {
        let Some(addon) = tree.mods.get(mod_node.mod_idx) else {
            continue;
        };
        if mod_node.files.is_empty() {
            continue;
        }

        expected_addons += 1;
        let addon_path = addon.local_path.trim();
        let addon_dir_exists = !addon_path.is_empty() && Path::new(addon_path).is_dir();
        if addon_dir_exists {
            existing_addons += 1;
        } else {
            missing_addon_dirs += 1;
            if sample_missing_addon_dirs.len() < SAMPLE_LIMIT {
                sample_missing_addon_dirs.push(addon.local_path.clone());
            }
        }

        let mut addon_expected_files = 0usize;
        let mut addon_resolved_files = 0usize;
        for file_idx in &mod_node.files {
            let Some(file) = tree.files.get(*file_idx) else {
                continue;
            };
            expected_files += 1;
            addon_expected_files += 1;

            let path = Path::new(file.local_path.trim());
            let Ok(metadata) = path.metadata() else {
                missing_files += 1;
                if sample_missing_files.len() < SAMPLE_LIMIT {
                    sample_missing_files.push(file.local_path.clone());
                }
                continue;
            };

            if !metadata.is_file() {
                missing_files += 1;
                if sample_missing_files.len() < SAMPLE_LIMIT {
                    sample_missing_files.push(file.local_path.clone());
                }
                continue;
            }

            existing_files += 1;
            addon_resolved_files += 1;
            if metadata.len() == file.length {
                existing_files_with_expected_size += 1;
            }
        }

        // Classify problem addons and capture per-addon detail for diagnostics.
        // Healthy and partially-resolved addons are skipped (they cost nothing).
        // A folder that exists but resolves *none* of its expected files is
        // probed for on-disk content: substantial content means the addon was
        // downloaded and merely lives at unexpected paths (layout/case drift)
        // rather than being a missing download.
        let problem = if addon_expected_files == 0 {
            None
        } else if !addon_dir_exists {
            Some((AddonPathProblem::MissingDir, 0usize))
        } else if addon_resolved_files == 0 {
            let needed = addon_expected_files
                .saturating_mul(LAYOUT_MISMATCH_MIN_DISK_RATIO_NUMERATOR)
                .div_ceil(LAYOUT_MISMATCH_MIN_DISK_RATIO_DENOMINATOR)
                .max(1);
            // Count up to the full expected total so the logged on-disk count is
            // representative, while the layout decision still keys off `needed`.
            let disk_files = count_disk_files_capped(
                Path::new(addon_path),
                addon_expected_files,
                &mut sample_unresolved_disk_paths,
                SAMPLE_LIMIT,
            );
            if disk_files >= needed {
                addons_with_disk_content_unresolved += 1;
                Some((AddonPathProblem::LayoutMismatch, disk_files))
            } else {
                Some((AddonPathProblem::FilesMissing, disk_files))
            }
        } else {
            None
        };

        if let Some((problem, on_disk_files)) = problem
            && addon_diagnostics.len() < DIAGNOSTIC_ADDON_LIMIT
        {
            addon_diagnostics.push(AddonPathDiagnostic {
                name: addon.name.clone(),
                configured_path: addon.local_path.clone(),
                dir_exists: addon_dir_exists,
                expected_files: addon_expected_files,
                resolved_files: addon_resolved_files,
                on_disk_files,
                problem,
            });
        }
    }

    LocalPathAvailability {
        repo_root,
        root_exists,
        expected_addons,
        existing_addons,
        missing_addon_dirs,
        expected_files,
        existing_files,
        existing_files_with_expected_size,
        missing_files,
        addons_with_disk_content_unresolved,
        had_prior_local_state,
        sample_missing_addon_dirs,
        sample_missing_files,
        sample_unresolved_disk_paths,
        addon_diagnostics,
    }
}

/// Count regular files beneath `root` (recursively), stopping as soon as `cap`
/// files have been seen. Collects up to `sample_limit` real paths for
/// diagnostics. Bounded by [`LAYOUT_MISMATCH_ENTRY_BUDGET`] directory entries so
/// a deep or hostile tree cannot stall the preflight. Symlinks are treated as
/// neither file nor dir (skipped) to avoid following loops.
fn count_disk_files_capped(
    root: &Path,
    cap: usize,
    samples: &mut Vec<String>,
    sample_limit: usize,
) -> usize {
    if cap == 0 {
        return 0;
    }
    let mut count = 0usize;
    let mut entry_budget = LAYOUT_MISMATCH_ENTRY_BUDGET;
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(read_dir) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in read_dir.flatten() {
            if entry_budget == 0 {
                return count;
            }
            entry_budget -= 1;
            match entry.file_type() {
                Ok(file_type) if file_type.is_dir() => stack.push(entry.path()),
                Ok(file_type) if file_type.is_file() => {
                    count += 1;
                    if samples.len() < sample_limit {
                        samples.push(entry.path().display().to_string());
                    }
                    if count >= cap {
                        return count;
                    }
                }
                _ => {}
            }
        }
    }
    count
}

/// Return up to `limit` immediate child entry names of `dir` (directories
/// suffixed with `/`, sorted), with a trailing `…` marker when more exist.
/// Returns a single explanatory marker when the directory cannot be read. Used
/// only for diagnostic logging, so it is bounded and never fails.
fn sample_dir_entries(dir: &Path, limit: usize) -> Vec<String> {
    const GATHER_CAP: usize = 256;
    let read_dir = match std::fs::read_dir(dir) {
        Ok(read_dir) => read_dir,
        Err(err) => return vec![format!("<unreadable: {}>", err.kind())],
    };
    let mut names: Vec<String> = Vec::new();
    let mut more = false;
    for entry in read_dir.flatten() {
        if names.len() >= GATHER_CAP {
            more = true;
            break;
        }
        let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
        let name = entry.file_name().to_string_lossy().into_owned();
        names.push(if is_dir { format!("{name}/") } else { name });
    }
    names.sort();
    let truncated = more || names.len() > limit;
    names.truncate(limit);
    if truncated {
        names.push("…".to_string());
    }
    names
}

/// Search beneath `root` (bounded, recursive) for files whose lowercased name
/// matches one of `wanted_lower`, returning up to `sample_limit` real on-disk
/// paths. Locates files that the manifest expects inside an addon folder but that
/// actually live elsewhere under the repo root - e.g. cross-repo shared-folder
/// layouts where content sits outside the declared addon folder - so the log
/// shows where it went. Bounded by [`ROOT_LOCATE_ENTRY_BUDGET`] entries; symlinks
/// are skipped to avoid following loops. Diagnostic only: a basename match is a
/// hint (filenames can repeat across addons), never an automated resolution.
fn locate_files_under_root_by_name(
    root: &Path,
    wanted_lower: &[String],
    sample_limit: usize,
) -> Vec<String> {
    if wanted_lower.is_empty() || sample_limit == 0 {
        return Vec::new();
    }
    let mut found = Vec::new();
    let mut entry_budget = ROOT_LOCATE_ENTRY_BUDGET;
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(read_dir) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in read_dir.flatten() {
            if entry_budget == 0 {
                return found;
            }
            entry_budget -= 1;
            match entry.file_type() {
                Ok(file_type) if file_type.is_dir() => stack.push(entry.path()),
                Ok(file_type) if file_type.is_file() => {
                    let name = entry.file_name().to_string_lossy().to_lowercase();
                    if wanted_lower.iter().any(|wanted| wanted == &name) {
                        found.push(entry.path().display().to_string());
                        if found.len() >= sample_limit {
                            return found;
                        }
                    }
                }
                _ => {}
            }
        }
    }
    found
}

/// Log a bounded snapshot of the actual on-disk layout for the problem addons in
/// `summary`: the configured repo root's contents plus, per problem addon, its
/// path/state and the folder's real contents. Surfaces exactly where the files
/// live versus where the manifest expects them. Logged at warn since it is only
/// called for suspect situations.
fn log_problem_addon_disk_state(repo_name_or_url: &str, summary: &LocalPathAvailability) {
    let root_entries = sample_dir_entries(
        Path::new(summary.repo_root.trim()),
        DISK_SNAPSHOT_ENTRY_LIMIT,
    );
    warn!(
        "Local path disk state for {}: configured_root={} root_exists={} root_contents=[{}]",
        repo_name_or_url,
        sanitize_log_path_str(&summary.repo_root),
        summary.root_exists,
        root_entries.join(", ")
    );
    for diagnostic in &summary.addon_diagnostics {
        let contents = if diagnostic.dir_exists {
            sample_dir_entries(
                Path::new(diagnostic.configured_path.trim()),
                DISK_SNAPSHOT_ENTRY_LIMIT,
            )
            .join(", ")
        } else {
            String::from("<folder missing>")
        };
        warn!(
            "  addon='{}' problem={:?} configured_path={} dir_exists={} expected_files={} resolved_files={} on_disk_files={} contents=[{}]",
            diagnostic.name,
            diagnostic.problem,
            sanitize_log_path_str(&diagnostic.configured_path),
            diagnostic.dir_exists,
            diagnostic.expected_files,
            diagnostic.resolved_files,
            diagnostic.on_disk_files,
            contents
        );
    }

    // When expected files did not resolve inside their own addon folders, search
    // the repo root for those filenames. This captures cross-repo / shared-folder
    // redirection - content living outside the declared addon folder, which the
    // per-addon probe above cannot see - directly in the log, so the actual-vs-
    // expected layout is visible without a manual recursive listing.
    let wanted_lower: Vec<String> = summary
        .sample_missing_files
        .iter()
        .filter_map(|path| {
            Path::new(path.trim())
                .file_name()
                .map(|name| name.to_string_lossy().to_lowercase())
        })
        .collect();
    if !wanted_lower.is_empty() && summary.root_exists {
        let located = locate_files_under_root_by_name(
            Path::new(summary.repo_root.trim()),
            &wanted_lower,
            DISK_SNAPSHOT_ENTRY_LIMIT,
        );
        if located.is_empty() {
            warn!(
                "  expected files not found anywhere under repo root (searched {} name(s), budget {} entries): likely genuinely absent or outside the repo root",
                wanted_lower.len(),
                ROOT_LOCATE_ENTRY_BUDGET
            );
        } else {
            let sample = located
                .iter()
                .map(|path| sanitize_log_path_str(path))
                .collect::<Vec<_>>()
                .join(", ");
            warn!(
                "  expected files located elsewhere under repo root (content present at unexpected paths): [{}]",
                sample
            );
        }
    }
}

/// Log a bounded on-disk snapshot for a raw set of addon paths under a repo
/// root. Used by the sync pipeline's pre-download / suspect full-redownload
/// path, which works with paths rather than a [`LocalPathAvailability`]. For a
/// missing addon folder it lists the parent so the actual layout is visible.
pub(crate) fn log_addon_path_disk_state(
    repo_name_or_url: &str,
    repo_root: &str,
    addon_paths: &[String],
) {
    let root = Path::new(repo_root.trim());
    let root_entries = sample_dir_entries(root, DISK_SNAPSHOT_ENTRY_LIMIT);
    info!(
        "Pre-download disk state for {}: configured_root={} root_exists={} root_contents=[{}]",
        repo_name_or_url,
        sanitize_log_path_str(repo_root),
        root.is_dir(),
        root_entries.join(", ")
    );
    for path in addon_paths.iter().take(DIAGNOSTIC_ADDON_LIMIT) {
        let trimmed = path.trim();
        if trimmed.is_empty() {
            continue;
        }
        let dir = Path::new(trimmed);
        let exists = dir.is_dir();
        let contents = if exists {
            sample_dir_entries(dir, DISK_SNAPSHOT_ENTRY_LIMIT).join(", ")
        } else {
            match dir.parent() {
                Some(parent) => format!(
                    "<missing; parent {} contains: {}>",
                    sanitize_log_path_str(&parent.display().to_string()),
                    sample_dir_entries(parent, DISK_SNAPSHOT_ENTRY_LIMIT).join(", ")
                ),
                None => String::from("<missing>"),
            }
        };
        info!(
            "  addon_path={} exists={} contents=[{}]",
            sanitize_log_path_str(path),
            exists,
            contents
        );
    }
}

pub(crate) fn suspect_local_path_mismatch(summary: &LocalPathAvailability) -> bool {
    if summary.expected_files < SUSPECT_MIN_FILES {
        return false;
    }

    if !summary.root_exists {
        return true;
    }

    // Empty existing roots are valid fresh download destinations, even when DB
    // state exists for the same remote repository at another local path.
    if summary.looks_like_empty_download_destination() {
        return false;
    }

    // Layout/path mismatch: addon folders contain files on disk, but the
    // manifest's expected files do not resolve at their declared paths. The
    // content was downloaded and merely lives at unexpected paths (case/layout
    // drift), so a (re)download would be wrong regardless of the missing ratio
    // or prior-download evidence. This probe only fires when real on-disk
    // content was confirmed, so it cannot mask a genuine never-downloaded repo.
    if summary.layout_mismatch_suspected() {
        return true;
    }

    // 100% of files missing is only a path-mismatch signal if the repo was previously
    // downloaded (it regressed from a known-good state). A never-downloaded repo in a shared
    // space - root exists because sibling repos populated it, but this repo's files were never
    // fetched - is a legitimate first install, not a misconfigured path. Let it proceed.
    if summary.missing_files == summary.expected_files {
        return summary.had_prior_local_state;
    }

    summary
        .missing_files
        .saturating_mul(SUSPECT_MISSING_FILE_RATIO_DENOMINATOR)
        >= summary
            .expected_files
            .saturating_mul(SUSPECT_MISSING_FILE_RATIO_NUMERATOR)
        && summary
            .missing_addon_dirs
            .saturating_mul(SUSPECT_MISSING_ADDON_RATIO_DENOMINATOR)
            >= summary
                .expected_addons
                .saturating_mul(SUSPECT_MISSING_ADDON_RATIO_NUMERATOR)
}

pub(crate) fn format_local_path_mismatch_message(
    repo_name_or_url: &str,
    summary: &LocalPathAvailability,
) -> String {
    // Layout/path mismatch: the content is on disk but under unexpected
    // names/subfolders. The configured path is correct, so the guidance differs
    // from the "wrong folder" case - re-pointing the path would not help.
    if summary.layout_mismatch_suspected() {
        let sample = summary
            .sample_unresolved_disk_paths
            .iter()
            .filter(|path| !path.trim().is_empty())
            .take(3)
            .map(|path| sanitize_log_path_str(path))
            .collect::<Vec<_>>()
            .join("; ");
        let sample_suffix = if sample.is_empty() {
            String::new()
        } else {
            format!(" Files present on disk include: {sample}.")
        };
        return format!(
            "Repository check paused for {repo_name_or_url}: {} addon folder(s) under the configured path ({}) contain files on disk, but the expected files were not found at their listed paths ({}/{} resolved). This looks like a layout or path-casing mismatch (content present under unexpected names/subfolders), not a missing download, so Foxy paused instead of flagging a redownload.{}",
            summary.addons_with_disk_content_unresolved,
            sanitize_log_path_str(&summary.repo_root),
            summary.existing_files,
            summary.expected_files,
            sample_suffix
        );
    }

    let sample = summary
        .sample_missing_addon_dirs
        .iter()
        .chain(summary.sample_missing_files.iter())
        .filter(|path| !path.trim().is_empty())
        .take(3)
        .map(|path| sanitize_log_path_str(path))
        .collect::<Vec<_>>()
        .join("; ");
    let sample_suffix = if sample.is_empty() {
        String::new()
    } else {
        format!(" Sample missing paths: {sample}.")
    };

    format!(
        "Repository check paused for {repo_name_or_url}: local files were not found at the configured path ({}). Found {}/{} expected files and {}/{} addon folders. Verify that the repository local path points to the folder that directly contains the addon folders before starting a redownload.{}",
        sanitize_log_path_str(&summary.repo_root),
        summary.existing_files,
        summary.expected_files,
        summary.existing_addons,
        summary.expected_addons,
        sample_suffix
    )
}

pub(crate) fn log_local_path_availability(repo_name_or_url: &str, summary: &LocalPathAvailability) {
    let missing_addons = summary
        .sample_missing_addon_dirs
        .iter()
        .map(|path| sanitize_log_path_str(path))
        .collect::<Vec<_>>()
        .join("; ");
    let missing_files = summary
        .sample_missing_files
        .iter()
        .map(|path| sanitize_log_path_str(path))
        .collect::<Vec<_>>()
        .join("; ");
    let unresolved_disk_files = summary
        .sample_unresolved_disk_paths
        .iter()
        .map(|path| sanitize_log_path_str(path))
        .collect::<Vec<_>>()
        .join("; ");

    let log_message = format!(
        "Repository local verification summary: repo={} root={} root_exists={} expected_addons={} existing_addons={} missing_addon_dirs={} expected_files={} existing_files={} existing_files_with_expected_size={} missing_files={} missing_ratio={}% addons_with_disk_content_unresolved={}; missing_addon_samples=[{}] missing_file_samples=[{}] unresolved_disk_samples=[{}]",
        repo_name_or_url,
        sanitize_log_path_str(&summary.repo_root),
        summary.root_exists,
        summary.expected_addons,
        summary.existing_addons,
        summary.missing_addon_dirs,
        summary.expected_files,
        summary.existing_files,
        summary.existing_files_with_expected_size,
        summary.missing_files,
        summary.missing_file_percent(),
        summary.addons_with_disk_content_unresolved,
        missing_addons,
        missing_files,
        unresolved_disk_files
    );

    if suspect_local_path_mismatch(summary) {
        warn!("{log_message}");
        // When the situation is suspect (layout mismatch or a large missing
        // ratio that could drive a near-full redownload), follow up with a
        // bounded on-disk layout snapshot so the actual vs expected paths are
        // captured in the logs.
        log_problem_addon_disk_state(repo_name_or_url, summary);
    } else {
        info!("{log_message}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::models::model_tree::ModNode;
    use crate::core::models::modification::FoxyMod;
    use crate::core::models::modification_file::FoxyModFile;
    use crate::core::models::repository::FoxyRepository;
    use std::fs;
    use tempfile::tempdir;

    fn test_tree(root: &Path, addon_names: &[&str], files_per_addon: usize) -> Tree {
        let mut mods = Vec::new();
        let mut files = Vec::new();
        let mut mod_nodes = Vec::new();
        for (addon_idx, addon_name) in addon_names.iter().enumerate() {
            let mut file_indices = Vec::new();
            let addon_path = root.join(addon_name);
            mods.push(FoxyMod {
                id: addon_idx as u64 + 1,
                name: (*addon_name).to_string(),
                local_path: addon_path.display().to_string(),
                enabled: true,
                ..Default::default()
            });

            for file_idx in 0..files_per_addon {
                let file_name = format!("file_{file_idx}.pbo");
                let local_path = addon_path.join(&file_name);
                file_indices.push(files.len());
                files.push(FoxyModFile {
                    id: files.len() as u64 + 1,
                    name: file_name,
                    local_path: local_path.display().to_string(),
                    length: 4,
                    ..Default::default()
                });
            }

            mod_nodes.push(ModNode {
                mod_idx: addon_idx,
                files: file_indices,
            });
        }

        Tree {
            repositories: vec![FoxyRepository {
                local_path: root.display().to_string(),
                ..FoxyRepository::default_for_tests()
            }],
            mods,
            files,
            mod_nodes,
            ..Default::default()
        }
    }

    trait TestRepositoryDefaults {
        fn default_for_tests() -> Self;
    }

    impl TestRepositoryDefaults for FoxyRepository {
        fn default_for_tests() -> Self {
            Self {
                id: 1,
                name: "Repo".to_string(),
                remote_url: "https://example.invalid/repo/".to_string(),
                local_path: String::new(),
                image: String::new(),
                local_checksum: String::new(),
                local_content_hash: String::new(),
                remote_checksum: String::new(),
                foxy_mode: Default::default(),
            }
        }
    }

    #[test]
    fn summarizes_all_files_present() {
        let dir = tempdir().unwrap();
        let tree = test_tree(dir.path(), &["@a", "@b"], 10);
        for addon in &tree.mods {
            fs::create_dir_all(&addon.local_path).unwrap();
        }
        for file in &tree.files {
            fs::write(&file.local_path, b"test").unwrap();
        }

        let summary = summarize_local_path_availability(&tree);

        assert!(summary.root_exists);
        assert_eq!(summary.expected_addons, 2);
        assert_eq!(summary.existing_addons, 2);
        assert_eq!(summary.expected_files, 20);
        assert_eq!(summary.existing_files, 20);
        assert_eq!(summary.existing_files_with_expected_size, 20);
        assert_eq!(summary.missing_files, 0);
        assert!(!suspect_local_path_mismatch(&summary));
    }

    /// Mark a tree as previously downloaded by stamping a local checksum on the repository row.
    fn with_prior_local_state(mut tree: Tree) -> Tree {
        if let Some(repo) = tree.repositories.first_mut() {
            repo.local_checksum = "PRIORLOCALCHECKSUM".to_string();
        }
        tree
    }

    #[test]
    fn all_files_missing_under_empty_root_is_allowed_as_fresh_destination() {
        let dir = tempdir().unwrap();
        let tree = with_prior_local_state(test_tree(dir.path(), &["@a", "@b"], 10));

        let summary = summarize_local_path_availability(&tree);

        assert!(summary.root_exists);
        assert!(summary.had_prior_local_state);
        assert_eq!(summary.missing_addon_dirs, 2);
        assert_eq!(summary.missing_files, 20);
        assert!(summary.looks_like_empty_download_destination());
        assert!(!suspect_local_path_mismatch(&summary));
    }

    #[test]
    fn all_files_missing_without_prior_state_is_not_suspect() {
        // A never-downloaded repo in an existing (shared) root must not be flagged as a
        // path mismatch - it is a legitimate first install.
        let dir = tempdir().unwrap();
        let tree = test_tree(dir.path(), &["@a", "@b"], 10);

        let summary = summarize_local_path_availability(&tree);

        assert!(summary.root_exists);
        assert!(!summary.had_prior_local_state);
        assert_eq!(summary.missing_files, 20);
        assert_eq!(summary.missing_files, summary.expected_files);
        assert!(!suspect_local_path_mismatch(&summary));
    }

    #[test]
    fn detects_missing_root() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("missing");
        let tree = test_tree(&root, &["@a", "@b"], 10);

        let summary = summarize_local_path_availability(&tree);

        assert!(!summary.root_exists);
        assert!(suspect_local_path_mismatch(&summary));
    }

    #[test]
    fn mixed_present_and_missing_is_not_suspect() {
        let dir = tempdir().unwrap();
        let tree = test_tree(dir.path(), &["@a", "@b"], 10);
        fs::create_dir_all(&tree.mods[0].local_path).unwrap();
        for file in tree.files.iter().take(10) {
            fs::write(&file.local_path, b"test").unwrap();
        }

        let summary = summarize_local_path_availability(&tree);

        assert_eq!(summary.existing_addons, 1);
        assert_eq!(summary.existing_files, 10);
        assert_eq!(summary.missing_files, 10);
        assert!(!suspect_local_path_mismatch(&summary));
    }

    #[test]
    fn addon_directory_without_files_counts_as_file_missing() {
        let dir = tempdir().unwrap();
        let tree = with_prior_local_state(test_tree(dir.path(), &["@a", "@b"], 10));
        for addon in &tree.mods {
            fs::create_dir_all(&addon.local_path).unwrap();
        }

        let summary = summarize_local_path_availability(&tree);

        assert_eq!(summary.existing_addons, 2);
        assert_eq!(summary.missing_addon_dirs, 0);
        assert_eq!(summary.missing_files, 20);
        // Empty addon folders hold no on-disk content, so this is a genuine
        // missing download, not a layout mismatch.
        assert_eq!(summary.addons_with_disk_content_unresolved, 0);
        assert!(!summary.layout_mismatch_suspected());
        assert!(suspect_local_path_mismatch(&summary));
    }

    #[test]
    fn detects_layout_mismatch_when_content_present_under_unexpected_paths() {
        // The manifest expects files directly under @a/file_N.pbo, but on disk
        // they live under @a/real_layout/*.pbo - present, just not where the
        // manifest points. This is the shared-space / layout-drift scenario.
        let dir = tempdir().unwrap();
        let tree = test_tree(dir.path(), &["@a", "@b"], 10);
        for addon in &tree.mods {
            let real_subdir = Path::new(&addon.local_path).join("real_layout");
            fs::create_dir_all(&real_subdir).unwrap();
            for i in 0..10 {
                fs::write(real_subdir.join(format!("on_disk_{i}.pbo")), b"data").unwrap();
            }
        }

        let summary = summarize_local_path_availability(&tree);

        assert_eq!(summary.existing_addons, 2);
        assert_eq!(
            summary.existing_files, 0,
            "no file resolves at its declared path"
        );
        assert_eq!(summary.missing_files, 20);
        assert_eq!(summary.addons_with_disk_content_unresolved, 2);
        assert!(summary.layout_mismatch_suspected());
        assert!(suspect_local_path_mismatch(&summary));
        assert!(!summary.sample_unresolved_disk_paths.is_empty());
    }

    #[test]
    fn partial_resolution_is_not_layout_mismatch() {
        // Some expected files resolve at their declared paths - a normal partial
        // download/update, not a layout mismatch.
        let dir = tempdir().unwrap();
        let tree = test_tree(dir.path(), &["@a"], 10);
        fs::create_dir_all(&tree.mods[0].local_path).unwrap();
        for file in tree.files.iter().take(3) {
            fs::write(&file.local_path, b"data").unwrap();
        }

        let summary = summarize_local_path_availability(&tree);

        assert_eq!(summary.existing_files, 3);
        assert_eq!(summary.addons_with_disk_content_unresolved, 0);
        assert!(!summary.layout_mismatch_suspected());
    }

    #[test]
    fn empty_addon_folder_is_not_layout_mismatch() {
        let dir = tempdir().unwrap();
        let tree = test_tree(dir.path(), &["@a"], 10);
        fs::create_dir_all(&tree.mods[0].local_path).unwrap();

        let summary = summarize_local_path_availability(&tree);

        assert_eq!(summary.addons_with_disk_content_unresolved, 0);
        assert!(!summary.layout_mismatch_suspected());
    }

    #[test]
    fn sparse_disk_content_below_half_is_not_layout_mismatch() {
        // The folder holds far fewer files than the addon expects (e.g. leftover
        // junk), so it is not treated as the addon present under a different
        // layout.
        let dir = tempdir().unwrap();
        let tree = test_tree(dir.path(), &["@a"], 10);
        let addon_dir = Path::new(&tree.mods[0].local_path);
        fs::create_dir_all(addon_dir).unwrap();
        // 2 stray files vs 10 expected → below the half threshold (needs 5).
        fs::write(addon_dir.join("leftover_a.txt"), b"x").unwrap();
        fs::write(addon_dir.join("leftover_b.txt"), b"x").unwrap();

        let summary = summarize_local_path_availability(&tree);

        assert_eq!(summary.addons_with_disk_content_unresolved, 0);
        assert!(!summary.layout_mismatch_suspected());
    }

    // ── count_disk_files_capped ─────────────────────────────────────────

    #[test]
    fn count_disk_files_capped_stops_at_cap() {
        let dir = tempdir().unwrap();
        for i in 0..10 {
            fs::write(dir.path().join(format!("f{i}.bin")), b"x").unwrap();
        }
        let mut samples = Vec::new();
        let count = count_disk_files_capped(dir.path(), 3, &mut samples, 2);
        assert_eq!(count, 3);
        assert_eq!(samples.len(), 2);
    }

    #[test]
    fn count_disk_files_capped_recurses_subdirs() {
        let dir = tempdir().unwrap();
        let sub = dir.path().join("addons");
        fs::create_dir_all(&sub).unwrap();
        fs::write(sub.join("a.pbo"), b"x").unwrap();
        fs::write(sub.join("b.pbo"), b"x").unwrap();
        let mut samples = Vec::new();
        let count = count_disk_files_capped(dir.path(), 100, &mut samples, 5);
        assert_eq!(count, 2);
    }

    #[test]
    fn count_disk_files_capped_zero_cap_returns_zero() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("f.bin"), b"x").unwrap();
        let mut samples = Vec::new();
        assert_eq!(count_disk_files_capped(dir.path(), 0, &mut samples, 5), 0);
    }

    #[test]
    fn count_disk_files_capped_missing_dir_returns_zero() {
        let dir = tempdir().unwrap();
        let missing = dir.path().join("does_not_exist");
        let mut samples = Vec::new();
        assert_eq!(count_disk_files_capped(&missing, 5, &mut samples, 5), 0);
    }

    // ── addon_diagnostics + sample_dir_entries ──────────────────────────

    #[test]
    fn diagnostics_capture_layout_mismatch_and_missing_dir() {
        let dir = tempdir().unwrap();
        // @a: folder present with content under an unexpected subfolder (layout
        // mismatch). @b: folder absent entirely (missing dir).
        let tree = test_tree(dir.path(), &["@a", "@b"], 6);
        let real_subdir = Path::new(&tree.mods[0].local_path).join("real_layout");
        fs::create_dir_all(&real_subdir).unwrap();
        for i in 0..6 {
            fs::write(real_subdir.join(format!("on_disk_{i}.pbo")), b"data").unwrap();
        }

        let summary = summarize_local_path_availability(&tree);

        assert_eq!(summary.addon_diagnostics.len(), 2);
        let a = summary
            .addon_diagnostics
            .iter()
            .find(|d| d.name == "@a")
            .expect("@a diagnostic");
        assert_eq!(a.problem, AddonPathProblem::LayoutMismatch);
        assert!(a.dir_exists);
        assert_eq!(a.expected_files, 6);
        assert_eq!(a.resolved_files, 0);
        assert!(a.on_disk_files >= 3);
        let b = summary
            .addon_diagnostics
            .iter()
            .find(|d| d.name == "@b")
            .expect("@b diagnostic");
        assert_eq!(b.problem, AddonPathProblem::MissingDir);
        assert!(!b.dir_exists);
        assert_eq!(b.on_disk_files, 0);
    }

    #[test]
    fn diagnostics_mark_empty_folder_as_files_missing() {
        let dir = tempdir().unwrap();
        let tree = test_tree(dir.path(), &["@a"], 6);
        fs::create_dir_all(&tree.mods[0].local_path).unwrap(); // empty folder

        let summary = summarize_local_path_availability(&tree);

        assert_eq!(summary.addon_diagnostics.len(), 1);
        assert_eq!(
            summary.addon_diagnostics[0].problem,
            AddonPathProblem::FilesMissing
        );
        assert_eq!(summary.addon_diagnostics[0].on_disk_files, 0);
    }

    #[test]
    fn diagnostics_skip_fully_resolved_addons() {
        let dir = tempdir().unwrap();
        let tree = test_tree(dir.path(), &["@a"], 4);
        fs::create_dir_all(&tree.mods[0].local_path).unwrap();
        for file in &tree.files {
            fs::write(&file.local_path, b"data").unwrap();
        }

        let summary = summarize_local_path_availability(&tree);

        assert!(summary.addon_diagnostics.is_empty());
    }

    #[test]
    fn sample_dir_entries_lists_and_marks_dirs() {
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path().join("addons")).unwrap();
        fs::write(dir.path().join("meta.cpp"), b"x").unwrap();

        let entries = sample_dir_entries(dir.path(), 10);

        assert!(entries.contains(&"addons/".to_string()));
        assert!(entries.contains(&"meta.cpp".to_string()));
    }

    #[test]
    fn sample_dir_entries_caps_and_marks_truncation() {
        let dir = tempdir().unwrap();
        for i in 0..10 {
            fs::write(dir.path().join(format!("f{i}.bin")), b"x").unwrap();
        }

        let entries = sample_dir_entries(dir.path(), 3);

        assert_eq!(entries.len(), 4); // 3 entries + the "…" marker
        assert_eq!(entries.last().unwrap(), "…");
    }

    #[test]
    fn sample_dir_entries_unreadable_returns_marker() {
        let dir = tempdir().unwrap();
        let missing = dir.path().join("nope");

        let entries = sample_dir_entries(&missing, 5);

        assert_eq!(entries.len(), 1);
        assert!(entries[0].starts_with("<unreadable"));
    }

    #[test]
    fn locate_files_under_root_finds_matches_in_sibling_dirs() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        // Declared addon folder exists but is empty; the file actually lives in a
        // sibling/shared folder elsewhere under the root (the redirection case).
        fs::create_dir_all(root.join("@addon/addons")).unwrap();
        fs::create_dir_all(root.join("shared/@addon")).unwrap();
        fs::write(root.join("shared/@addon/data.pbo"), b"x").unwrap();

        let found = locate_files_under_root_by_name(root, &["data.pbo".to_string()], 5);

        assert_eq!(found.len(), 1);
        let normalized = found[0].replace('\\', "/").to_lowercase();
        assert!(normalized.ends_with("shared/@addon/data.pbo"));
    }

    #[test]
    fn locate_files_under_root_matches_case_insensitively() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("Data.PBO"), b"x").unwrap();

        let found = locate_files_under_root_by_name(dir.path(), &["data.pbo".to_string()], 5);

        assert_eq!(found.len(), 1);
    }

    #[test]
    fn locate_files_under_root_empty_wanted_returns_empty() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("a.pbo"), b"x").unwrap();

        assert!(locate_files_under_root_by_name(dir.path(), &[], 5).is_empty());
    }

    #[test]
    fn locate_files_under_root_respects_sample_limit() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join("a")).unwrap();
        fs::create_dir_all(root.join("b")).unwrap();
        fs::write(root.join("a/x.pbo"), b"1").unwrap();
        fs::write(root.join("b/x.pbo"), b"2").unwrap();

        let found = locate_files_under_root_by_name(root, &["x.pbo".to_string()], 1);

        assert_eq!(found.len(), 1);
    }

    /// Build a `LocalPathAvailability` directly so boundary conditions can be
    /// exercised without touching the filesystem.
    fn availability(
        root_exists: bool,
        expected_addons: usize,
        missing_addon_dirs: usize,
        expected_files: usize,
        missing_files: usize,
        had_prior_local_state: bool,
    ) -> LocalPathAvailability {
        LocalPathAvailability {
            repo_root: "C:/repo".to_string(),
            root_exists,
            expected_addons,
            existing_addons: expected_addons.saturating_sub(missing_addon_dirs),
            missing_addon_dirs,
            expected_files,
            existing_files: expected_files.saturating_sub(missing_files),
            existing_files_with_expected_size: expected_files.saturating_sub(missing_files),
            missing_files,
            addons_with_disk_content_unresolved: 0,
            had_prior_local_state,
            sample_missing_addon_dirs: Vec::new(),
            sample_missing_files: Vec::new(),
            sample_unresolved_disk_paths: Vec::new(),
            addon_diagnostics: Vec::new(),
        }
    }

    // ── missing_file_percent ────────────────────────────────────────────

    #[test]
    fn missing_file_percent_zero_expected_returns_zero() {
        let summary = availability(true, 0, 0, 0, 0, false);
        assert_eq!(summary.missing_file_percent(), 0);
    }

    #[test]
    fn missing_file_percent_half() {
        let summary = availability(true, 2, 0, 10, 5, true);
        assert_eq!(summary.missing_file_percent(), 50);
    }

    #[test]
    fn missing_file_percent_all_missing() {
        let summary = availability(true, 2, 2, 20, 20, true);
        assert_eq!(summary.missing_file_percent(), 100);
    }

    #[test]
    fn missing_file_percent_none_missing() {
        let summary = availability(true, 2, 0, 40, 0, true);
        assert_eq!(summary.missing_file_percent(), 0);
    }

    #[test]
    fn missing_file_percent_truncates_toward_zero() {
        // 1/3 ≈ 33.33% → integer floor 33
        let summary = availability(true, 1, 0, 3, 1, true);
        assert_eq!(summary.missing_file_percent(), 33);
    }

    // ── format_local_path_mismatch_message ──────────────────────────────

    #[test]
    fn format_message_includes_counts_and_repo_name() {
        let summary = availability(true, 10, 6, 30, 27, true);
        let message = format_local_path_mismatch_message("My Repo", &summary);
        assert!(message.contains("My Repo"));
        // existing_files/expected_files
        assert!(message.contains("3/30 expected files"));
        // existing_addons/expected_addons
        assert!(message.contains("4/10 addon folders"));
    }

    #[test]
    fn format_message_without_samples_has_no_sample_suffix() {
        let summary = availability(false, 5, 5, 25, 25, true);
        let message = format_local_path_mismatch_message("repo", &summary);
        assert!(!message.contains("Sample missing paths"));
    }

    #[test]
    fn format_message_with_samples_lists_them() {
        let mut summary = availability(false, 5, 5, 25, 25, true);
        summary.sample_missing_addon_dirs = vec!["C:/repo/@ace".to_string()];
        summary.sample_missing_files = vec!["C:/repo/@ace/addons/main.pbo".to_string()];
        let message = format_local_path_mismatch_message("repo", &summary);
        assert!(message.contains("Sample missing paths:"));
        assert!(message.contains("@ace"));
    }

    #[test]
    fn format_message_blank_sample_paths_are_skipped() {
        let mut summary = availability(false, 5, 5, 25, 25, true);
        summary.sample_missing_addon_dirs = vec!["   ".to_string(), String::new()];
        let message = format_local_path_mismatch_message("repo", &summary);
        assert!(!message.contains("Sample missing paths"));
    }

    #[test]
    fn format_message_caps_samples_at_three() {
        let mut summary = availability(false, 8, 8, 40, 40, true);
        summary.sample_missing_addon_dirs = vec![
            "C:/repo/@a".to_string(),
            "C:/repo/@b".to_string(),
            "C:/repo/@c".to_string(),
            "C:/repo/@d".to_string(),
        ];
        let message = format_local_path_mismatch_message("repo", &summary);
        assert!(message.contains("@a"));
        assert!(message.contains("@b"));
        assert!(message.contains("@c"));
        assert!(!message.contains("@d"));
    }

    // ── suspect_local_path_mismatch boundaries ──────────────────────────

    #[test]
    fn suspect_ignores_repositories_below_minimum_file_count() {
        // 19 expected files (< SUSPECT_MIN_FILES) all missing, root missing,
        // prior state present - still not suspect because the sample is too small.
        let summary = availability(false, 1, 1, 19, 19, true);
        assert!(!suspect_local_path_mismatch(&summary));
    }

    #[test]
    fn suspect_at_exact_minimum_file_count_with_missing_root() {
        let summary = availability(false, 2, 2, 20, 20, true);
        assert!(suspect_local_path_mismatch(&summary));
    }

    #[test]
    fn suspect_when_root_missing_even_with_prior_state_false() {
        // Root absent is a hard signal regardless of prior-download evidence,
        // as long as the minimum file count is met.
        let summary = availability(false, 2, 0, 20, 0, false);
        assert!(suspect_local_path_mismatch(&summary));
    }

    #[test]
    fn suspect_all_missing_in_existing_addon_folders_requires_prior_state() {
        let with_state = availability(true, 4, 0, 40, 40, true);
        let without_state = availability(true, 4, 0, 40, 40, false);
        assert!(!with_state.looks_like_empty_download_destination());
        assert!(suspect_local_path_mismatch(&with_state));
        assert!(!suspect_local_path_mismatch(&without_state));
    }

    #[test]
    fn suspect_ninety_percent_files_and_half_addons_is_suspect() {
        // 18/20 files missing (90%), 5/10 addon dirs missing (50%).
        let summary = availability(true, 10, 5, 20, 18, true);
        assert!(suspect_local_path_mismatch(&summary));
    }

    #[test]
    fn suspect_false_when_addon_ratio_below_half() {
        // 18/20 files missing (90%) but only 4/10 addon dirs missing (40%).
        let summary = availability(true, 10, 4, 20, 18, true);
        assert!(!suspect_local_path_mismatch(&summary));
    }

    #[test]
    fn suspect_false_when_file_ratio_below_ninety() {
        // 17/20 files missing (85%), addon dirs all missing.
        let summary = availability(true, 10, 10, 20, 17, true);
        assert!(!suspect_local_path_mismatch(&summary));
    }

    #[test]
    fn suspect_partial_missing_under_existing_root_is_not_suspect() {
        // Half the files missing under an existing root is a normal update, not
        // a path mismatch.
        let summary = availability(true, 10, 5, 40, 20, true);
        assert!(!suspect_local_path_mismatch(&summary));
    }

    #[test]
    fn suspect_true_on_layout_mismatch_even_below_ratio() {
        // Only 30% of files missing (well below the 90% ratio) with every addon
        // folder present, but on-disk content is unresolved → still suspect.
        let mut summary = availability(true, 4, 0, 40, 12, true);
        summary.addons_with_disk_content_unresolved = 1;
        assert!(suspect_local_path_mismatch(&summary));
    }

    #[test]
    fn suspect_layout_mismatch_overrides_all_missing_without_prior_state() {
        // 100% missing with no prior-download evidence would normally be treated
        // as a legitimate first install; confirmed on-disk content flips it to a
        // layout mismatch so a redownload is not flagged.
        let mut summary = availability(true, 2, 0, 20, 20, false);
        summary.addons_with_disk_content_unresolved = 2;
        assert!(suspect_local_path_mismatch(&summary));
    }

    #[test]
    fn format_message_layout_mismatch_explains_layout_not_wrong_path() {
        let mut summary = availability(true, 2, 0, 20, 20, false);
        summary.addons_with_disk_content_unresolved = 2;
        summary.sample_unresolved_disk_paths = vec!["C:/repo/@a/real/on_disk.pbo".to_string()];
        let message = format_local_path_mismatch_message("My Repo", &summary);
        assert!(message.contains("My Repo"));
        assert!(message.contains("layout or path-casing mismatch"));
        assert!(message.contains("on_disk.pbo"));
        // Must not give the misleading "re-point the configured path" guidance,
        // since the path is correct here.
        assert!(!message.contains("points to the folder that directly contains"));
    }
}
