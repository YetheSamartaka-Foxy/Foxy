use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::core::steam;
use crate::core::utils::fs_safety::resolve_child_dir_case_insensitive;
use crate::ui::types::{
    Repository, RepositoryServer, SettingsViewState, split_additional_launch_params,
};

use super::generic::{RunScriptTemplateVars, render_run_script_template};
use super::{
    DirectorySetting, GameBuildTarget, GameCapabilities, GameDetectCtx, GameLaunchCtx, GameModule,
    GameSettingsSchema, LaunchCommand, LaunchError, LaunchPlan, ResolvedMod, TextSetting,
    ToggleSetting, reforger, workshop,
};

pub const GENERIC_GAME_ID: &str = "generic";
pub const GENERIC_INSTALL_DIR_SETTING_ID: &str = "generic_directory";
const DEFAULT_MANIFEST_NAME: &str = "foxy_mods.txt";

pub struct GenericGameModule;

/// The parts of a user-configured game that Foxy cannot know statically.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct GenericGameConfig {
    pub install_dir: String,
    pub executable: String,
    pub steam_app_id: Option<u32>,
    pub launch_template: String,
    pub mods_manifest: String,
}

pub fn config_from_settings(settings: &SettingsViewState) -> GenericGameConfig {
    GenericGameConfig {
        install_dir: settings.generic_directory.trim().to_string(),
        executable: settings.generic_executable.trim().to_string(),
        steam_app_id: parse_app_id(&settings.generic_steam_app_id),
        launch_template: settings.generic_launch_template.trim().to_string(),
        mods_manifest: settings.generic_mods_manifest.trim().to_string(),
    }
}

pub fn parse_app_id(value: &str) -> Option<u32> {
    value.trim().parse::<u32>().ok().filter(|id| *id > 0)
}

impl GameModule for GenericGameModule {
    fn id(&self) -> &'static str {
        GENERIC_GAME_ID
    }

    fn display_name(&self) -> &str {
        "Generic game"
    }

    fn capabilities(&self) -> GameCapabilities {
        GameCapabilities {
            repository_sync: true,
            repository_launch: true,
            steam_workshop: true,
            client_side_addons: false,
            direct_download: true,
            extra_files: true,
            profiles: false,
            foxy_config_export: true,
            teamspeak3_plugins: false,
            creator_dlc: false,
            join_addon_preflight: false,
        }
    }

    fn detect_install_dir(&self, _ctx: &GameDetectCtx) -> Option<PathBuf> {
        None
    }

    fn validate_install_dir(&self, path: &Path) -> bool {
        path.is_dir()
    }

    fn build_launch(
        &self,
        plan: &LaunchPlan,
        ctx: &GameLaunchCtx,
    ) -> Result<LaunchCommand, LaunchError> {
        let settings = ctx.settings.ok_or(LaunchError::GameNotConfigured)?;
        let config = config_from_settings(settings);
        build_generic_launch(plan, ctx, &config, true).map(|built| built.command)
    }

    fn build_launch_preview(
        &self,
        plan: &LaunchPlan,
        ctx: &GameLaunchCtx,
    ) -> Result<LaunchCommand, LaunchError> {
        let settings = ctx.settings.ok_or(LaunchError::GameNotConfigured)?;
        build_generic_launch(plan, ctx, &config_from_settings(settings), false)
            .map(|built| built.command)
    }

    fn launch_write_targets(&self, plan: &LaunchPlan, ctx: &GameLaunchCtx) -> Vec<PathBuf> {
        let Some(settings) = ctx.settings else { return Vec::new() };
        build_generic_launch(plan, ctx, &config_from_settings(settings), false)
            .ok().and_then(|built| built.manifest).map(|manifest| vec![manifest.path]).unwrap_or_default()
    }

    fn settings_schema(&self) -> GameSettingsSchema {
        GameSettingsSchema {
            directories: vec![DirectorySetting {
                id: GENERIC_INSTALL_DIR_SETTING_ID,
                label: "Game Directory",
                help: Some("The folder Foxy launches from and writes the mods manifest into."),
                auto_detect: false,
                is_install_dir: true,
            }],
            texts: vec![
                TextSetting {
                    id: "generic_executable",
                    label: "Executable",
                    help: Some(
                        "Executable to launch, relative to the game directory or an absolute path.",
                    ),
                    placeholder: "game.exe",
                },
                TextSetting {
                    id: "generic_steam_app_id",
                    label: "Steam App ID",
                    help: Some(
                        "Optional. Set it to launch through Steam and to manage this game's Steam Workshop items.",
                    ),
                    placeholder: "0",
                },
                TextSetting {
                    id: "generic_launch_template",
                    label: "Launch Arguments",
                    help: Some(
                        "Argument template. Tokens: {mods}, {mods_sep=;}, {mod_ids}, {manifest_name}, {extra}.",
                    ),
                    placeholder: "-mod=\"{mods_sep=;}\"",
                },
                TextSetting {
                    id: "generic_mods_manifest",
                    label: "Mods Manifest File",
                    help: Some(
                        "Optional file name written into the game directory with one enabled mod path per line.",
                    ),
                    placeholder: DEFAULT_MANIFEST_NAME,
                },
            ],
            toggles: vec![ToggleSetting {
                id: "check_steam_running_before_launch",
                label: "Check Steam is running before launching",
                help: "Before launching, warn if Steam is not running and offer to launch it.",
            }],
        }
    }

    fn build_repository_launch_plan(
        &self,
        _settings: &SettingsViewState,
        repo: &Repository,
        _server: Option<&RepositoryServer>,
    ) -> Result<LaunchPlan, LaunchError> {
        Ok(build_repository_launch_plan(repo))
    }

    fn install_dir_from_settings<'a>(&self, settings: &'a SettingsViewState) -> &'a str {
        &settings.generic_directory
    }

    fn steam_app_id_from_settings(&self, settings: &SettingsViewState) -> Option<u32> {
        parse_app_id(&settings.generic_steam_app_id)
    }
}

/// A repository whose `repo.json` names a game build starts the game from that
/// downloaded folder; any other repository starts the configured install.
/// Enabled addon folders are only offered to the launch template as `{mods}`.
pub fn build_repository_launch_plan(repo: &Repository) -> LaunchPlan {
    let repo_path = Path::new(repo.path.trim());
    let game_build = repo.game_build.as_ref().map(|build| GameBuildTarget {
        dir: resolve_child_dir_case_insensitive(repo_path, &build.addon)
            .unwrap_or_else(|| repo_path.join(&build.addon)),
        executable: build.executable.clone(),
    });
    let mut mods = Vec::new();
    for (addon, enabled) in repo.addons.iter().chain(repo.optional_addons.iter()) {
        if !*enabled {
            continue;
        }
        match resolve_child_dir_case_insensitive(repo_path, addon) {
            Some(dir) => mods.push(addon_mod(addon, &dir)),
            None => log::error!(
                "Generic game addon folder not found in repository: {}",
                addon
            ),
        }
    }
    for (addon, enabled, path) in &repo.external_addons {
        if !*enabled {
            continue;
        }
        match reforger::resolve_external_addon_dir(addon, path) {
            Some(dir) => mods.push(addon_mod(addon, &dir)),
            None => log::error!(
                "External generic game addon folder not found: addon={}",
                addon
            ),
        }
    }

    LaunchPlan {
        launch_args: split_additional_launch_params(&repo.additional_params),
        mods,
        server: None,
        game_build,
    }
}

fn addon_mod(addon: &str, dir: &Path) -> ResolvedMod {
    ResolvedMod {
        id: addon.to_string(),
        path: Some(dir.display().to_string()),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GenericGameLaunchBuild {
    pub command: LaunchCommand,
    pub manifest: Option<GenericManifestPreview>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GenericManifestPreview {
    pub file_name: String,
    pub path: PathBuf,
    pub content: String,
    pub written: bool,
}

pub fn build_generic_launch(
    plan: &LaunchPlan,
    ctx: &GameLaunchCtx,
    config: &GenericGameConfig,
    write_manifest: bool,
) -> Result<GenericGameLaunchBuild, LaunchError> {
    // A downloaded build is not the Steam install, so it never goes through Steam.
    let (install_path, executable_setting, steam_app_id) = match &plan.game_build {
        Some(build) => (build.dir.as_path(), build.executable.as_str(), None),
        None => {
            let install_dir = ctx.install_dir.trim();
            if install_dir.is_empty() {
                return Err(LaunchError::InstallDirNotConfigured);
            }
            (
                Path::new(install_dir),
                config.executable.as_str(),
                config.steam_app_id,
            )
        }
    };
    if !install_path.is_dir() {
        return Err(LaunchError::InstallDirMissing);
    }
    if executable_setting.is_empty() {
        return Err(LaunchError::GameNotConfigured);
    }
    let executable = resolve_executable(install_path, executable_setting)
        .ok_or(LaunchError::InstallDirInvalid)?;

    let mut manifest = None;
    let mut manifest_name = None;
    if !config.mods_manifest.is_empty() {
        let file_name = sanitize_manifest_name(&config.mods_manifest)
            .ok_or(LaunchError::LaunchPreparationFailed)?;
        let content = render_manifest(&plan.mods);
        let path = install_path.join(&file_name);
        let written = if write_manifest {
            fs::write(&path, content.as_bytes()).map_err(|err| {
                log::warn!("Failed to write generic game mods manifest: {}", err);
                LaunchError::LaunchPreparationFailed
            })?;
            true
        } else {
            false
        };
        manifest_name = Some(file_name.clone());
        manifest = Some(GenericManifestPreview {
            file_name,
            path,
            content,
            written,
        });
    }

    let vars = RunScriptTemplateVars {
        mods: plan
            .mods
            .iter()
            .map(|entry| entry.launch_value().to_string())
            .collect(),
        mod_ids: plan.mods.iter().map(|entry| entry.id.clone()).collect(),
        manifest_name: manifest_name.as_deref(),
        profile: None,
        extra: plan.launch_args.clone(),
    };
    let mut game_args = plan.launch_args.clone();
    game_args.extend(split_launch_args(&render_run_script_template(
        &config.launch_template,
        &vars,
    )));

    let (program, mut args) = match steam_app_id {
        Some(app_id) => {
            let executable_names = [executable_setting];
            let launch = steam::steam_app_launch_command(
                app_id,
                install_path,
                &executable_names,
                ctx.steam_directory,
            )
            .ok_or(LaunchError::LauncherUnavailable)?;
            (launch.program, launch.args)
        }
        None => (executable, Vec::new()),
    };
    args.extend(game_args);

    Ok(GenericGameLaunchBuild {
        command: LaunchCommand {
            program,
            args,
            cwd: Some(install_path.to_path_buf()),
        },
        manifest,
    })
}

fn resolve_executable(install_path: &Path, executable: &str) -> Option<PathBuf> {
    let candidate = Path::new(executable);
    let resolved = if candidate.is_absolute() {
        candidate.to_path_buf()
    } else {
        install_path.join(candidate)
    };
    resolved.is_file().then_some(resolved)
}

fn sanitize_manifest_name(value: &str) -> Option<String> {
    let trimmed = value.trim();
    crate::core::utils::fs_safety::is_safe_child_path(trimmed)
        .then(|| trimmed.to_string())
        .filter(|name| !name.contains('/') && !name.contains('\\'))
}

fn render_manifest(mods: &[ResolvedMod]) -> String {
    mods.iter()
        .map(|item| item.launch_value().to_string())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Split a rendered argument template the way a shell would, so a player can
/// quote a path that contains spaces.
pub fn split_launch_args(rendered: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut has_token = false;
    for ch in rendered.chars() {
        match quote {
            Some(open) if ch == open => quote = None,
            Some(_) => current.push(ch),
            None if ch == '"' || ch == '\'' => {
                quote = Some(ch);
                has_token = true;
            }
            None if ch.is_whitespace() => {
                if has_token {
                    args.push(std::mem::take(&mut current));
                    has_token = false;
                }
            }
            None => {
                current.push(ch);
                has_token = true;
            }
        }
    }
    if has_token {
        args.push(current);
    }
    args
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct GenericWorkshopIssue {
    pub item_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub error: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GenericWorkshopLaunchPlan {
    pub plan: LaunchPlan,
    pub issues: Vec<GenericWorkshopIssue>,
}

/// Resolve the enabled Workshop items of a user-configured game to the folders
/// Foxy would hand the game. Unlike the Warhammer plan this keeps whole item
/// directories, because a generic game has no known content layout.
pub fn build_workshop_launch_plan(
    space_dir: &Path,
    app_id: u32,
    steam_directory: &str,
    include_disabled: bool,
    launch_args: Vec<String>,
) -> Result<GenericWorkshopLaunchPlan, String> {
    let store = workshop::load_store(space_dir)?;
    let mut entries: Vec<&workshop::SteamWorkshopItem> = store
        .entries
        .iter()
        .filter(|entry| entry.app_id == app_id)
        .filter(|entry| include_disabled || entry.enabled)
        .collect();
    entries.sort_by_key(|entry| workshop::launch_order_key(entry));

    let mut mods = Vec::new();
    let mut issues = Vec::new();
    for entry in entries {
        match workshop::resolve_launch_path(space_dir, app_id, &entry.item_id, steam_directory) {
            Ok(resolution) => mods.push(ResolvedMod {
                id: entry.item_id.clone(),
                path: Some(resolution.path),
            }),
            Err(error) => issues.push(GenericWorkshopIssue {
                item_id: entry.item_id.clone(),
                title: entry.title.clone(),
                error,
            }),
        }
    }

    Ok(GenericWorkshopLaunchPlan {
        plan: LaunchPlan {
            launch_args,
            mods,
            server: None,
            game_build: None,
        },
        issues,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(install_dir: &Path, template: &str, manifest: &str) -> GenericGameConfig {
        GenericGameConfig {
            install_dir: install_dir.display().to_string(),
            executable: "game.exe".to_string(),
            steam_app_id: None,
            launch_template: template.to_string(),
            mods_manifest: manifest.to_string(),
        }
    }

    #[test]
    fn splits_quoted_launch_arguments() {
        assert_eq!(
            split_launch_args("-mod=\"C:/Mods/My Mod;C:/Mods/Other\" -skipIntro"),
            vec!["-mod=C:/Mods/My Mod;C:/Mods/Other", "-skipIntro"]
        );
        assert_eq!(split_launch_args("   "), Vec::<String>::new());
    }

    #[test]
    fn builds_direct_launch_with_template_and_manifest() {
        let dir = tempfile::tempdir().expect("temp dir");
        let install_dir = dir.path().join("game");
        fs::create_dir_all(&install_dir).expect("install dir");
        fs::write(install_dir.join("game.exe"), "").expect("exe");
        let plan = LaunchPlan {
            launch_args: vec!["-window".to_string()],
            mods: vec![
                ResolvedMod {
                    id: "111".to_string(),
                    path: Some("D:/Mods/a".to_string()),
                },
                ResolvedMod {
                    id: "222".to_string(),
                    path: Some("D:/Mods/b".to_string()),
                },
            ],
            server: None,
            game_build: None,
        };
        let install_dir_text = install_dir.display().to_string();
        let ctx = GameLaunchCtx {
            install_dir: &install_dir_text,
            steam_directory: "",
            settings: None,
        };
        let config = config(
            &install_dir,
            "-mod=\"{mods_sep=;}\" {manifest_name}",
            "mods.txt",
        );

        let built = build_generic_launch(&plan, &ctx, &config, true).expect("launch");

        assert_eq!(built.command.program, install_dir.join("game.exe"));
        assert_eq!(
            built.command.args,
            vec!["-window", "-mod=D:/Mods/a;D:/Mods/b", "mods.txt"]
        );
        let manifest = built.manifest.expect("manifest");
        assert!(manifest.written);
        assert_eq!(
            fs::read_to_string(install_dir.join("mods.txt")).expect("manifest file"),
            "D:/Mods/a\nD:/Mods/b"
        );
    }

    #[test]
    fn refuses_launch_without_an_executable() {
        let dir = tempfile::tempdir().expect("temp dir");
        let install_dir_text = dir.path().display().to_string();
        let ctx = GameLaunchCtx {
            install_dir: &install_dir_text,
            steam_directory: "",
            settings: None,
        };
        let mut config = config(dir.path(), "", "");
        config.executable.clear();

        assert_eq!(
            build_generic_launch(
                &LaunchPlan {
                    launch_args: Vec::new(),
                    mods: Vec::new(),
                    server: None,
                    game_build: None,
                },
                &ctx,
                &config,
                false
            ),
            Err(LaunchError::GameNotConfigured)
        );
    }

    #[test]
    fn repository_launch_plan_offers_enabled_addon_folders_as_mods() {
        let repo_root = tempfile::tempdir().expect("repo");
        for name in ["MyGame", "Optional", "Disabled"] {
            fs::create_dir_all(repo_root.path().join(name)).expect("addon dir");
        }
        let external = tempfile::tempdir().expect("external");
        fs::create_dir_all(external.path().join("Extra")).expect("external dir");
        let repo = Repository {
            path: repo_root.path().display().to_string(),
            addons: vec![
                ("MyGame".to_string(), true),
                ("Disabled".to_string(), false),
                ("Missing".to_string(), true),
            ],
            optional_addons: vec![("Optional".to_string(), true)],
            external_addons: vec![(
                "Extra".to_string(),
                true,
                external.path().display().to_string(),
            )],
            additional_params: "-windowed -fps 60".to_string(),
            ..Repository::default()
        };

        let plan = GenericGameModule
            .build_repository_launch_plan(&SettingsViewState::default(), &repo, None)
            .expect("plan");

        assert_eq!(plan.launch_args, vec!["-windowed", "-fps", "60"]);
        assert_eq!(
            plan.mods,
            vec![
                addon_mod("MyGame", &repo_root.path().join("MyGame")),
                addon_mod("Optional", &repo_root.path().join("Optional")),
                addon_mod("Extra", &external.path().join("Extra")),
            ]
        );
        assert!(plan.server.is_none());
        assert!(plan.game_build.is_none());
        let capabilities = GenericGameModule.capabilities();
        assert!(capabilities.repository_sync && capabilities.repository_launch);
    }

    #[test]
    fn game_build_repository_launches_its_own_executable_without_steam() {
        let repo_root = tempfile::tempdir().expect("repo");
        let build_dir = repo_root.path().join("MyGame");
        fs::create_dir_all(build_dir.join("bin")).expect("build dir");
        fs::write(build_dir.join("bin").join("game.exe"), "").expect("exe");
        let repo = Repository {
            path: repo_root.path().display().to_string(),
            game_build: Some(crate::ui::types::RepositoryGameBuild {
                addon: "MyGame".to_string(),
                executable: "bin/game.exe".to_string(),
            }),
            additional_params: "-windowed".to_string(),
            ..Repository::default()
        };
        let plan = build_repository_launch_plan(&repo);
        assert_eq!(
            plan.game_build,
            Some(GameBuildTarget {
                dir: build_dir.clone(),
                executable: "bin/game.exe".to_string(),
            })
        );

        let ctx = GameLaunchCtx {
            install_dir: "",
            steam_directory: "",
            settings: None,
        };
        let mut configured = config(Path::new(""), "", "");
        configured.executable = "Other.exe".to_string();
        configured.steam_app_id = Some(480);
        let built = build_generic_launch(&plan, &ctx, &configured, false).expect("launch");
        assert_eq!(built.command.program, build_dir.join("bin/game.exe"));
        assert_eq!(built.command.args, vec!["-windowed"]);
        assert_eq!(built.command.cwd.as_deref(), Some(build_dir.as_path()));

        fs::remove_dir_all(&build_dir).expect("remove build");
        assert_eq!(
            build_generic_launch(
                &build_repository_launch_plan(&repo),
                &ctx,
                &configured,
                false
            ),
            Err(LaunchError::InstallDirMissing)
        );
    }

    #[test]
    fn manifest_name_must_stay_inside_the_game_directory() {
        assert_eq!(
            sanitize_manifest_name("mods.txt").as_deref(),
            Some("mods.txt")
        );
        assert!(sanitize_manifest_name("../mods.txt").is_none());
        assert!(sanitize_manifest_name("sub/mods.txt").is_none());
    }
}
