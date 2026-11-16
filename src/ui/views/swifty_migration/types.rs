#[derive(Debug, Clone)]
pub struct SwiftyDetectedRepo {
    pub name: String,

    pub address: String,

    pub mod_folder: String,

    pub parameters: String,

    pub autocheck: bool,

    pub selected: bool,
}

#[derive(Debug, Clone, Default)]
pub struct SwiftyGlobalSettings {
    pub arma_path: String,

    pub temp_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DerivedUrls {
    pub base_url: String,

    pub updater_url: String,

    pub space_url: String,
}

#[derive(Debug, Clone, Default)]
pub struct SwiftyMigrationState {
    pub detected_repos: Vec<SwiftyDetectedRepo>,

    pub scan_complete: bool,

    pub scan_error: Option<String>,

    pub import_done: bool,

    pub imported_count: usize,

    pub detected_updater_url: String,

    pub detected_space_url: String,

    pub space_import_failed: bool,

    pub global_settings: SwiftyGlobalSettings,
}
