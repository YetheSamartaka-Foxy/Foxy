//! Types used by the Swifty-to-Foxy migration wizard.

/// A single repository discovered in a Swifty configuration file.
#[derive(Debug, Clone)]
pub struct SwiftyDetectedRepo {
    /// Human-readable name from the Swifty config.
    pub name: String,
    /// Original remote address (e.g. `http://server:8080/mods/RepoName`).
    pub address: String,
    /// Local mod folder that Swifty used for this repository.
    pub mod_folder: String,
    /// Raw launch-parameter string from Swifty (e.g. `"-skipIntro -noSplash -world=empty"`).
    pub parameters: String,
    /// Whether Swifty was set to auto-check this repository.
    pub autocheck: bool,
    /// Whether the user wants to import this repository.
    pub selected: bool,
}

/// Global Swifty settings that are not per-repository.
#[derive(Debug, Clone, Default)]
pub struct SwiftyGlobalSettings {
    /// Arma 3 installation directory configured in Swifty.
    pub arma_path: String,
    /// Temporary download directory configured in Swifty.
    pub temp_path: String,
}

/// Derived URL set produced from a Swifty repository address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DerivedUrls {
    /// Base URL up to and including the parent path segment
    /// (e.g. `http://server:8080/mods/`).
    pub base_url: String,
    /// Candidate updater endpoint (`{base_url}Foxy`).
    pub updater_url: String,
    /// Candidate repository-space manifest (`{base_url}repository_space.json`).
    pub space_url: String,
}

/// Transient state that lives only while the migration view is open.
#[derive(Debug, Clone, Default)]
pub struct SwiftyMigrationState {
    /// Repositories detected from Swifty data files.
    pub detected_repos: Vec<SwiftyDetectedRepo>,
    /// Whether scanning has been attempted.
    pub scan_complete: bool,
    /// Human-readable scan error, if any.
    pub scan_error: Option<String>,
    /// Whether the import operation has been executed in this session.
    pub import_done: bool,
    /// Number of repositories successfully imported in the last run.
    pub imported_count: usize,
    /// Auto-detected updater URL (editable by the user before import).
    pub detected_updater_url: String,
    /// Auto-detected repository-space manifest URL (editable by the user before import).
    pub detected_space_url: String,
    /// Whether the repository-space import was attempted and failed.
    pub space_import_failed: bool,
    /// Global Swifty settings (Arma path, temp path) - editable before import.
    pub global_settings: SwiftyGlobalSettings,
}
