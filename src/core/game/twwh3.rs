use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::ui::types::SettingsViewState;

use super::generic::{
    GenericLaunchBuild, GenericRunScriptConfig, GenericRunScriptModule, ManifestEncoding,
    ModsManifestConfig,
};
use super::{
    GameCapabilities, GameDetectCtx, GameLaunchCtx, GameModule, GameSettingsSchema, LaunchCommand,
    LaunchError, LaunchPlan, ResolvedMod,
};

pub const TWWH3_GAME_ID: &str = "twwh3";
pub const TWWH3_APP_ID: u32 = 1142710;
pub const TWWH3_EXECUTABLE: &str = "Warhammer3.exe";
pub const TWWH3_INSTALL_DIR_SETTING_ID: &str = "twwh3_directory";
pub const TWWH3_USED_MODS_FILE: &str = "used_mods.txt";
pub const TWWH3_FALLBACK_MODS_FILE: &str = "my_mods.txt";

const TWWH3_MANIFEST: ModsManifestConfig = ModsManifestConfig {
    primary_file_name: TWWH3_USED_MODS_FILE,
    fallback_file_name: TWWH3_FALLBACK_MODS_FILE,
    encoding: ManifestEncoding::Utf8,
};

const TWWH3_CONFIG: GenericRunScriptConfig = GenericRunScriptConfig {
    id: TWWH3_GAME_ID,
    display_name: "Total War: WARHAMMER III",
    install_dir_setting_id: TWWH3_INSTALL_DIR_SETTING_ID,
    install_dir_label: "Total War: WARHAMMER III Directory",
    install_dir_help: Some("Foxy writes the mods manifest into this game directory before launch."),
    steam_app_id: Some(TWWH3_APP_ID),
    executable_names: &[TWWH3_EXECUTABLE],
    default_steam_install_dirs: &["Total War WARHAMMER III"],
    capabilities: GameCapabilities {
        repository_sync: false,
        repository_launch: false,
        steam_workshop: true,
        direct_download: true,
        extra_files: true,
        // Profiles are still a repository-launch concept
        // (`RepositoryProfile`); there is no game-space profile store yet.
        profiles: false,
        foxy_config_export: true,
        teamspeak3_plugins: false,
    },
    manifest: Some(TWWH3_MANIFEST),
    arg_templates: &["{manifest_name};"],
};

pub struct TotalWarWarhammer3Module;

impl TotalWarWarhammer3Module {
    fn generic(&self) -> GenericRunScriptModule {
        GenericRunScriptModule::new(&TWWH3_CONFIG)
    }

    pub fn build_launch_with_manifest_mode(
        &self,
        plan: &LaunchPlan,
        ctx: &GameLaunchCtx,
        write_manifest: bool,
    ) -> Result<GenericLaunchBuild, LaunchError> {
        self.generic()
            .build_launch_with_manifest_mode(plan, ctx, write_manifest)
    }
}

impl GameModule for TotalWarWarhammer3Module {
    fn id(&self) -> &'static str {
        TWWH3_GAME_ID
    }

    fn display_name(&self) -> &str {
        TWWH3_CONFIG.display_name
    }

    fn capabilities(&self) -> GameCapabilities {
        TWWH3_CONFIG.capabilities
    }

    fn detect_install_dir(&self, ctx: &GameDetectCtx) -> Option<PathBuf> {
        self.generic().detect_install_dir(ctx)
    }

    fn validate_install_dir(&self, path: &Path) -> bool {
        self.generic().validate_install_dir(path)
    }

    fn build_launch(
        &self,
        plan: &LaunchPlan,
        ctx: &GameLaunchCtx,
    ) -> Result<LaunchCommand, LaunchError> {
        self.generic().build_launch(plan, ctx)
    }

    fn settings_schema(&self) -> GameSettingsSchema {
        self.generic().settings_schema()
    }

    fn install_dir_from_settings<'a>(&self, settings: &'a SettingsViewState) -> &'a str {
        &settings.twwh3_directory
    }

    fn steam_app_id(&self) -> Option<u32> {
        Some(TWWH3_APP_ID)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct WorkshopPackResolution {
    pub item_id: String,
    pub title: Option<String>,
    pub item_path: String,
    pub pack_path: String,
    pub frozen: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct WorkshopPackIssue {
    pub item_id: String,
    pub title: Option<String>,
    pub error: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Twwh3WorkshopLaunchPlan {
    pub plan: LaunchPlan,
    pub packs: Vec<WorkshopPackResolution>,
    pub issues: Vec<WorkshopPackIssue>,
}

pub fn build_workshop_launch_plan(
    space_dir: &Path,
    steam_directory: &str,
    include_disabled: bool,
    save_name: Option<&str>,
) -> Result<Twwh3WorkshopLaunchPlan, String> {
    let store = super::workshop::load_store(space_dir)?;
    let mut mods = Vec::new();
    let mut packs = Vec::new();
    let mut issues = Vec::new();

    for item in store
        .entries
        .iter()
        .filter(|entry| entry.app_id == TWWH3_APP_ID)
        .filter(|entry| include_disabled || entry.enabled)
    {
        let resolution = match super::workshop::resolve_launch_path(
            space_dir,
            TWWH3_APP_ID,
            &item.item_id,
            steam_directory,
        ) {
            Ok(resolution) => resolution,
            Err(error) => {
                issues.push(WorkshopPackIssue {
                    item_id: item.item_id.clone(),
                    title: item.title.clone(),
                    error,
                });
                continue;
            }
        };
        let item_path = PathBuf::from(&resolution.path);
        let pack_files = pack_files_in_item_dir(&item_path);
        if pack_files.is_empty() {
            issues.push(WorkshopPackIssue {
                item_id: item.item_id.clone(),
                title: item.title.clone(),
                error: format!("No .pack files found in {}", item_path.display()),
            });
            continue;
        }

        for pack_path in pack_files {
            let id = format!(
                "{}:{}",
                item.item_id,
                pack_path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or_default()
            );
            let pack_path_text = pack_path.display().to_string();
            mods.push(ResolvedMod {
                id,
                path: Some(pack_path_text.clone()),
            });
            packs.push(WorkshopPackResolution {
                item_id: item.item_id.clone(),
                title: item.title.clone(),
                item_path: resolution.path.clone(),
                pack_path: pack_path_text,
                frozen: resolution.frozen,
            });
        }
    }

    Ok(Twwh3WorkshopLaunchPlan {
        plan: LaunchPlan {
            launch_args: save_game_launch_args(save_name),
            mods,
            server: None,
        },
        packs,
        issues,
    })
}

pub fn save_game_launch_args(save_name: Option<&str>) -> Vec<String> {
    let Some(save_name) = save_name.map(str::trim).filter(|value| !value.is_empty()) else {
        return Vec::new();
    };
    vec![
        "game_startup_mode".to_string(),
        "campaign_load".to_string(),
        save_name.to_string(),
        ";".to_string(),
    ]
}

pub fn pack_files_in_item_dir(item_dir: &Path) -> Vec<PathBuf> {
    let mut packs = Vec::new();
    let Ok(entries) = fs::read_dir(item_dir) else {
        return packs;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        if path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("pack"))
        {
            packs.push(path);
        }
    }
    packs.sort_by(|left, right| {
        left.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase()
            .cmp(
                &right
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or_default()
                    .to_ascii_lowercase(),
            )
    });
    packs
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_workshop_pack(steam_root: &Path, item_id: &str, pack_name: &str) -> PathBuf {
        let item_dir = steam_root
            .join("steamapps")
            .join("workshop")
            .join("content")
            .join(TWWH3_APP_ID.to_string())
            .join(item_id);
        fs::create_dir_all(&item_dir).expect("item dir");
        let pack_path = item_dir.join(pack_name);
        fs::write(&pack_path, "pack").expect("pack file");
        pack_path
    }

    #[test]
    fn pack_files_in_item_dir_returns_sorted_pack_files_only() {
        let dir = tempfile::tempdir().expect("temp dir");
        fs::write(dir.path().join("zeta.pack"), "").expect("pack");
        fs::write(dir.path().join("alpha.PACK"), "").expect("pack");
        fs::write(dir.path().join("readme.txt"), "").expect("text");

        let names: Vec<String> = pack_files_in_item_dir(dir.path())
            .iter()
            .map(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or_default()
                    .to_string()
            })
            .collect();

        assert_eq!(names, vec!["alpha.PACK", "zeta.pack"]);
    }

    #[test]
    fn workshop_launch_plan_resolves_enabled_pack_files_and_save_args() {
        let space = tempfile::tempdir().expect("space");
        let steam = tempfile::tempdir().expect("steam");
        let first_pack = create_workshop_pack(steam.path(), "111", "alpha.pack");
        create_workshop_pack(steam.path(), "222", "disabled.pack");
        super::super::workshop::upsert_item(
            space.path(),
            TWWH3_APP_ID,
            "111",
            Some("Alpha".to_string()),
            None,
            None,
            true,
        )
        .expect("first item");
        super::super::workshop::upsert_item(
            space.path(),
            TWWH3_APP_ID,
            "222",
            Some("Disabled".to_string()),
            None,
            None,
            false,
        )
        .expect("disabled item");

        let result = build_workshop_launch_plan(
            space.path(),
            &steam.path().display().to_string(),
            false,
            Some("Karl Franz"),
        )
        .expect("plan");

        assert!(result.issues.is_empty());
        assert_eq!(
            result.plan.launch_args,
            save_game_launch_args(Some("Karl Franz"))
        );
        assert_eq!(
            result.plan.mods,
            vec![ResolvedMod {
                id: "111:alpha.pack".to_string(),
                path: Some(first_pack.display().to_string()),
            }]
        );
        assert_eq!(result.packs.len(), 1);
        assert_eq!(result.packs[0].title.as_deref(), Some("Alpha"));
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn twwh3_module_builds_exact_arg_vector_and_manifest_preview() {
        let dir = tempfile::tempdir().expect("temp dir");
        let install_dir = dir.path().join("Total War WARHAMMER III");
        let workshop_dir = dir.path().join("workshop").join("111");
        fs::create_dir_all(&install_dir).expect("install dir");
        fs::create_dir_all(&workshop_dir).expect("workshop dir");
        fs::write(install_dir.join(TWWH3_EXECUTABLE), "").expect("exe");
        let pack_path = workshop_dir.join("alpha.pack");
        fs::write(&pack_path, "pack").expect("pack");
        let plan = LaunchPlan {
            launch_args: save_game_launch_args(Some("The Empire")),
            mods: vec![ResolvedMod {
                id: "111:alpha.pack".to_string(),
                path: Some(pack_path.display().to_string()),
            }],
            server: None,
        };
        let install_dir_text = install_dir.display().to_string();
        let ctx = GameLaunchCtx {
            install_dir: &install_dir_text,
            steam_directory: "",
        };

        let built = TotalWarWarhammer3Module
            .build_launch_with_manifest_mode(&plan, &ctx, false)
            .expect("launch");

        assert_eq!(built.command.program, install_dir.join(TWWH3_EXECUTABLE));
        assert_eq!(
            built.command.args,
            vec![
                "game_startup_mode",
                "campaign_load",
                "The Empire",
                ";",
                "used_mods.txt;",
            ]
        );
        let manifest = built.manifest.expect("manifest preview");
        assert!(!manifest.written);
        assert_eq!(manifest.file_name, TWWH3_USED_MODS_FILE);
        assert_eq!(
            manifest.content,
            format!(
                "add_working_directory \"{}\";\nmod \"alpha.pack\";",
                workshop_dir.display()
            )
        );
        assert!(!install_dir.join(TWWH3_USED_MODS_FILE).exists());
    }
}
