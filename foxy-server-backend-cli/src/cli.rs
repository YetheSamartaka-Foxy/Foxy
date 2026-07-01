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

#[derive(Parser)]
#[command(name = "foxy-server-backend-cli")]
#[command(about = "Generate Foxy-compatible repository structures for Arma 3 mod hosting")]
#[command(version)]
pub struct Cli {
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
        /// Optional Foxy app update source URL to write as repo.json appUpdateUrl
        #[arg(long)]
        app_update_url: Option<String>,
        /// Number of threads for parallel operations
        #[arg(long, default_value_t = default_threads())]
        threads: usize,
        /// Generation mode: foxy (BLAKE3, default), swifty (MD5, legacy), hybrid (both)
        #[arg(long, value_enum, default_value_t = GenerationMode::Foxy)]
        mode: GenerationMode,
    },
    /// Generate a blank repository config file
    New {
        /// Output path for the config file
        #[arg(default_value = "config.json")]
        output: PathBuf,
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
