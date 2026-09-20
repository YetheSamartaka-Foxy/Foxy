use clap::{Parser, Subcommand, ValueEnum};
use std::path::PathBuf;

/// Controls which hashing algorithm and manifest format the server CLI produces.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum GenerationMode {
    /// BLAKE3 hashing, foxy_addon.json per mod, foxy_addons.json at repo root (default).
    Foxy,
    /// MD5 hashing, mod.srf per mod - legacy Swifty-compatible output.
    Swifty,
    /// Generates both FoxyMode and SwiftyMode artifacts side by side.
    Hybrid,
}

/// How `create-space` materializes the mod folders of each repository.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum SpaceLayout {
    /// Copy every mod into every repository that lists it, exactly like running `create` per repository.
    Copy,
    /// Copy each distinct mod once into a shared pool folder and symlink it from every repository. Recommended: deduplicated on disk, and the output tree stays self-contained.
    Pool,
    /// Symlink each repository mod straight to its source folder without copying anything. Requires --yes: manifests are written into the source folders and any later change there silently breaks the published checksums.
    Link,
}

impl SpaceLayout {
    pub fn as_str(self) -> &'static str {
        match self {
            SpaceLayout::Copy => "copy",
            SpaceLayout::Pool => "pool",
            SpaceLayout::Link => "link",
        }
    }
}

#[derive(Parser)]
#[command(name = "foxy-server-backend-cli")]
#[command(
    about = "Generate Foxy-compatible repository structures for Arma 3 and Arma Reforger mod hosting"
)]
#[command(version = crate::build_info::clap_version())]
pub struct Cli {
    /// Print one machine-readable JSON result on stdout
    #[arg(long, global = true)]
    pub json: bool,
    #[arg(
        long,
        global = true,
        help = "Disable animated progress bar output (screen-reader friendly)"
    )]
    pub no_progress: bool,
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    /// Create a repository from a config file
    Create {
        /// Path to the repository config JSON
        config: PathBuf,
        /// Output directory for the generated repository
        output: PathBuf,
        /// Preview all generated file operations without writing output
        #[arg(long)]
        dry_run: bool,
        /// Reuse previously generated unchanged mods based on file size and modification time
        #[arg(long)]
        incremental: bool,
        /// Build beside the output and replace it after success; requires --yes
        #[arg(long, requires = "yes", conflicts_with = "incremental")]
        atomic: bool,
        /// Print additions, changes, removals, and estimated download bytes
        #[arg(long)]
        report: bool,
        /// Optional Foxy app update source URL to write as repo.json appUpdateUrl
        #[arg(long)]
        app_update_url: Option<String>,
        /// Number of threads for parallel operations
        #[arg(long, default_value_t = default_threads())]
        threads: usize,
        /// Generation mode: foxy (BLAKE3, default), swifty (MD5, legacy), hybrid (both)
        #[arg(long, value_enum, default_value_t = GenerationMode::Foxy)]
        mode: GenerationMode,
        /// Path prefix for each mod folder in the printed server launch line (e.g. "mods"); the -addonsDir root for Arma Reforger
        #[arg(long, default_value = "")]
        mod_line_prefix: String,
        /// Include optional mods in the printed server launch line
        #[arg(long)]
        mod_line_include_optional: bool,
        /// Omit root-level optionals folders from published mods; requires --yes
        #[arg(long)]
        prune_unused_optionals: bool,
        /// Accept removal of previously published optional files
        #[arg(long)]
        yes: bool,
        /// Copy every .bikey from the generated mods into a combined keys folder
        #[arg(long)]
        collect_keys: bool,
        /// Destination for the combined keys folder (default: <output>/keys, implies --collect-keys)
        #[arg(long, value_name = "DIR")]
        keys_output: Option<PathBuf>,
        /// Extra key file or directory to add to the combined keys folder (repeatable, implies --collect-keys)
        #[arg(long, value_name = "PATH")]
        additional_keys: Vec<PathBuf>,
    },
    /// Generate a blank repository config file
    New {
        /// Output path for the config file
        #[arg(default_value = "config.json")]
        output: PathBuf,
        /// Game the template is for: arma3 (default) or reforger
        #[arg(long, value_enum, default_value_t = crate::types::RepoGame::Arma3)]
        game: crate::types::RepoGame,
    },
    /// Create every repository of a repository space plus its repository_space.json in one pass
    CreateSpace {
        /// Path to the repository space config JSON (see `new-space`)
        config: PathBuf,
        /// Output directory for the whole space; each repository lands in its own subfolder
        output: PathBuf,
        /// How mod folders are stored: copy (per repository), pool (shared folder + symlinks, recommended), link (symlinks to the sources)
        #[arg(long, value_enum, default_value_t = SpaceLayout::Copy)]
        layout: SpaceLayout,
        /// Shared mod folder for --layout pool (default: <output>/pool)
        #[arg(long, value_name = "DIR")]
        pool_dir: Option<PathBuf>,
        /// Accept source manifest writes with link, output pruning, or pool cleanup
        #[arg(long)]
        yes: bool,
        /// Remove orphaned generated pool mods and their repository symlinks
        #[arg(long)]
        clean: bool,
        /// Preview generation, overwrites, pruning, and optional cleanup without writing
        #[arg(long)]
        dry_run: bool,
        /// Reuse previously generated unchanged mods based on file size and modification time
        #[arg(long)]
        incremental: bool,
        /// Build beside the output and replace it after success; requires --yes
        #[arg(long, requires = "yes", conflicts_with_all = ["incremental", "only", "pool_dir", "clean"])]
        atomic: bool,
        /// Print additions, changes, removals, and estimated download bytes
        #[arg(long)]
        report: bool,
        /// Regenerate only this repository folder (repeatable)
        #[arg(long, value_name = "FOLDER")]
        only: Vec<String>,
        /// Omit root-level optionals folders from published mods; requires --yes
        #[arg(long)]
        prune_unused_optionals: bool,
        /// Optional Foxy app update source URL for every generated repo.json (wins over config values)
        #[arg(long)]
        app_update_url: Option<String>,
        /// Number of threads for parallel operations
        #[arg(long, default_value_t = default_threads())]
        threads: usize,
        /// Generation mode: foxy (BLAKE3, default), swifty (MD5, legacy), hybrid (both)
        #[arg(long, value_enum, default_value_t = GenerationMode::Foxy)]
        mode: GenerationMode,
        /// Path prefix for each mod folder in the printed server launch lines (e.g. "mods"); the -addonsDir root for Arma Reforger
        #[arg(long, default_value = "")]
        mod_line_prefix: String,
        /// Include optional mods in the printed server launch lines
        #[arg(long)]
        mod_line_include_optional: bool,
        /// Copy every .bikey from all generated repositories into one combined keys folder
        #[arg(long)]
        collect_keys: bool,
        /// Destination for the combined keys folder (default: <output>/keys, implies --collect-keys)
        #[arg(long, value_name = "DIR")]
        keys_output: Option<PathBuf>,
        /// Extra key file or directory to add to the combined keys folder (repeatable, implies --collect-keys)
        #[arg(long, value_name = "PATH")]
        additional_keys: Vec<PathBuf>,
        /// Also write each repository's .bikey files (plus --additional-keys) into <output>/<folder>/keys so a server can symlink one repository's keys folder directly
        #[arg(long)]
        per_repo_keys: bool,
    },
    /// Generate a blank repository space config file
    NewSpace {
        /// Output path for the space config file
        #[arg(default_value = "space.json")]
        output: PathBuf,
    },
    /// Check source config and published names without hashing or writing files
    Validate {
        config: PathBuf,
        /// Interpret the input as a repository space config
        #[arg(long)]
        space: bool,
        /// Also check output path overlap and layout collisions
        #[arg(long)]
        output: Option<PathBuf>,
    },
    /// Verify generated repository or space content against its manifests
    Verify { output: PathBuf },
    /// Compare generated repository or space outputs
    Diff { old: PathBuf, new: PathBuf },
    /// Audit Arma 3 source keys and PBO signatures
    AuditKeys {
        config: PathBuf,
        /// Interpret the input as a repository space config
        #[arg(long)]
        space: bool,
        /// Fail if unsigned PBOs or duplicate key names are found
        #[arg(long)]
        strict: bool,
        /// Extra server key file or directory to include in the audit (repeatable)
        #[arg(long, value_name = "PATH")]
        additional_keys: Vec<PathBuf>,
    },
    /// Write a Reforger game.mods JSON fragment from a repository config
    ExportReforgerConfig {
        config: PathBuf,
        output: PathBuf,
        /// Include enabled optional mods
        #[arg(long)]
        include_optional: bool,
    },
    /// Create a fresh update manifest with changelog JSONs from a CHANGELOG.md
    SetupAppUpdater {
        /// Version to publish (e.g. "0.5.1")
        #[arg(long)]
        version: String,
        /// Path to Windows installer file (hash will be computed)
        #[arg(long)]
        windows_installer: Option<PathBuf>,
        /// Path to Linux installer file (hash will be computed)
        #[arg(long)]
        linux_installer: Option<PathBuf>,
        /// Path to Linux ARM64 installer file (hash will be computed)
        #[arg(long)]
        linux_aarch64_installer: Option<PathBuf>,
        /// Path to CHANGELOG.md to parse into per-version JSON files
        #[arg(long)]
        changelog: PathBuf,
        /// Output directory for the server root (foxy-app-updater.json + changelogs/)
        #[arg(long, default_value = ".")]
        output: PathBuf,
    },
    /// Add a new version to an existing manifest (preserves old versions)
    NewAppUpdate {
        /// New version to add (e.g. "0.5.2")
        #[arg(long)]
        version: String,
        /// Path to Windows installer file (hash will be computed)
        #[arg(long)]
        windows_installer: Option<PathBuf>,
        /// Path to Linux installer file (hash will be computed)
        #[arg(long)]
        linux_installer: Option<PathBuf>,
        /// Path to Linux ARM64 installer file (hash will be computed)
        #[arg(long)]
        linux_aarch64_installer: Option<PathBuf>,
        /// Path to CHANGELOG.md (only the matching version section will be extracted)
        #[arg(long)]
        changelog: PathBuf,
        /// Output directory containing foxy-app-updater.json
        #[arg(long, default_value = ".")]
        output: PathBuf,
    },
}

fn default_threads() -> usize {
    // Default to a single worker thread so wildcard expansion and manifest
    // emission produce deterministic output unless the user explicitly opts
    // into higher parallelism.
    1
}
