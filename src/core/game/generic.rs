use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use crate::core::steam;

use super::{
    DirectorySetting, GameCapabilities, GameDetectCtx, GameLaunchCtx, GameModule,
    GameSettingsSchema, LaunchCommand, LaunchError, LaunchPlan, ResolvedMod, ToggleSetting,
};

#[derive(Clone, Copy)]
pub struct GenericRunScriptConfig {
    pub id: &'static str,
    pub display_name: &'static str,
    pub install_dir_setting_id: &'static str,
    pub install_dir_label: &'static str,
    pub install_dir_help: Option<&'static str>,
    pub steam_app_id: Option<u32>,
    pub executable_names: &'static [&'static str],
    pub default_steam_install_dirs: &'static [&'static str],
    pub capabilities: GameCapabilities,
    pub manifest: Option<ModsManifestConfig>,
    pub arg_templates: &'static [&'static str],
}

#[derive(Clone, Copy)]
pub struct ModsManifestConfig {
    pub primary_file_name: &'static str,
    pub fallback_file_name: &'static str,
    pub encoding: ManifestEncoding,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ManifestEncoding {
    Utf8,
}

pub struct GenericRunScriptModule {
    config: &'static GenericRunScriptConfig,
}

impl GenericRunScriptModule {
    pub const fn new(config: &'static GenericRunScriptConfig) -> Self {
        Self { config }
    }

    pub fn build_launch_with_manifest_mode(
        &self,
        plan: &LaunchPlan,
        ctx: &GameLaunchCtx,
        write_manifest: bool,
    ) -> Result<GenericLaunchBuild, LaunchError> {
        let install_dir = ctx.install_dir.trim();
        if install_dir.is_empty() {
            return Err(LaunchError::InstallDirNotConfigured);
        }
        let install_path = Path::new(install_dir);
        if !install_path.exists() {
            return Err(LaunchError::InstallDirMissing);
        }
        if !self.validate_install_dir(install_path) {
            return Err(LaunchError::InstallDirInvalid);
        }

        // Resolve the launcher before writing the manifest so a launch that
        // cannot start never rewrites the game's mods manifest.
        let app_id = self
            .config
            .steam_app_id
            .ok_or(LaunchError::LauncherUnavailable)?;
        let launch = steam::steam_app_launch_command(
            app_id,
            install_path,
            self.config.executable_names,
            ctx.steam_directory,
        )
        .ok_or(LaunchError::LauncherUnavailable)?;

        let mut manifest = None;
        let mut manifest_name = None;
        if let Some(manifest_config) = self.config.manifest {
            let content = render_mods_manifest(&manifest_config, install_path, &plan.mods)
                .map_err(|err| {
                    log::warn!("Failed to render game mods manifest: {}", err);
                    LaunchError::LaunchPreparationFailed
                })?;
            let manifest_result = if write_manifest {
                write_mods_manifest(&manifest_config, install_path, content).map_err(|err| {
                    log::warn!("Failed to write game mods manifest: {}", err);
                    LaunchError::LaunchPreparationFailed
                })?
            } else {
                ManifestBuildResult {
                    file_name: manifest_config.primary_file_name.to_string(),
                    path: install_path.join(manifest_config.primary_file_name),
                    content,
                    written: false,
                }
            };
            manifest_name = Some(manifest_result.file_name.clone());
            manifest = Some(manifest_result);
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
        game_args.extend(render_arg_templates(self.config.arg_templates, &vars));

        let cwd = Some(install_path.to_path_buf());
        let mut args = launch.args;
        args.extend(game_args);

        Ok(GenericLaunchBuild {
            command: LaunchCommand {
                program: launch.program,
                args,
                cwd,
            },
            manifest,
        })
    }
}

impl GameModule for GenericRunScriptModule {
    fn id(&self) -> &'static str {
        self.config.id
    }

    fn display_name(&self) -> &str {
        self.config.display_name
    }

    fn capabilities(&self) -> GameCapabilities {
        self.config.capabilities
    }

    fn detect_install_dir(&self, ctx: &GameDetectCtx) -> Option<PathBuf> {
        steam::detect_steam_app_install_directory(
            ctx.steam_directory,
            self.config.steam_app_id?,
            self.config.default_steam_install_dirs,
            |path| self.validate_install_dir(path),
        )
    }

    fn validate_install_dir(&self, path: &Path) -> bool {
        path.is_dir()
            && self
                .config
                .executable_names
                .iter()
                .any(|name| path.join(name).is_file())
    }

    fn build_launch(
        &self,
        plan: &LaunchPlan,
        ctx: &GameLaunchCtx,
    ) -> Result<LaunchCommand, LaunchError> {
        self.build_launch_with_manifest_mode(plan, ctx, true)
            .map(|result| result.command)
    }

    fn settings_schema(&self) -> GameSettingsSchema {
        GameSettingsSchema {
            directories: vec![DirectorySetting {
                id: self.config.install_dir_setting_id,
                label: self.config.install_dir_label,
                help: self.config.install_dir_help,
                auto_detect: self.config.steam_app_id.is_some(),
                is_install_dir: true,
            }],
            toggles: vec![ToggleSetting {
                id: "check_steam_running_before_launch",
                label: "Check Steam is running before launching",
                help: "Before launching, warn if Steam is not running and offer to launch it.",
            }],
        }
    }

    fn steam_app_id(&self) -> Option<u32> {
        self.config.steam_app_id
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GenericLaunchBuild {
    pub command: LaunchCommand,
    pub manifest: Option<ManifestBuildResult>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ManifestBuildResult {
    pub file_name: String,
    pub path: PathBuf,
    pub content: String,
    pub written: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RunScriptTemplateVars<'a> {
    pub mods: Vec<String>,
    pub mod_ids: Vec<String>,
    pub manifest_name: Option<&'a str>,
    pub profile: Option<&'a str>,
    pub extra: Vec<String>,
}

pub fn render_arg_templates(templates: &[&str], vars: &RunScriptTemplateVars<'_>) -> Vec<String> {
    templates
        .iter()
        .map(|template| render_run_script_template(template, vars))
        .filter(|arg| !arg.trim().is_empty())
        .collect()
}

pub fn render_run_script_template(template: &str, vars: &RunScriptTemplateVars<'_>) -> String {
    let mut rendered = String::new();
    let mut rest = template;
    while let Some(start) = rest.find('{') {
        rendered.push_str(&rest[..start]);
        let after_start = &rest[start + 1..];
        let Some(end) = after_start.find('}') else {
            rendered.push_str(&rest[start..]);
            return rendered;
        };
        let token = &after_start[..end];
        rendered.push_str(&render_template_token(token, vars));
        rest = &after_start[end + 1..];
    }
    rendered.push_str(rest);
    rendered
}

fn render_template_token(token: &str, vars: &RunScriptTemplateVars<'_>) -> String {
    match token {
        "mods" => vars.mods.join(" "),
        "mod_ids" => vars.mod_ids.join(" "),
        "manifest_name" => vars.manifest_name.unwrap_or_default().to_string(),
        "profile" => vars.profile.unwrap_or_default().to_string(),
        "extra" => vars.extra.join(" "),
        _ => token
            .strip_prefix("mods_sep=")
            .map(|separator| vars.mods.join(separator))
            .unwrap_or_else(|| format!("{{{}}}", token)),
    }
}

pub fn render_mods_manifest(
    config: &ModsManifestConfig,
    install_dir: &Path,
    mods: &[ResolvedMod],
) -> Result<String, String> {
    match config.encoding {
        ManifestEncoding::Utf8 => {}
    }

    let data_dir = install_dir.join("data");
    let mut working_dirs = Vec::new();
    let mut seen_dirs = HashSet::new();
    let mut pack_lines = Vec::new();

    for item in mods {
        let path = item
            .path
            .as_deref()
            .map(PathBuf::from)
            .ok_or_else(|| format!("Mod {} has no pack path", item.id))?;
        let pack_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|name| !name.trim().is_empty())
            .ok_or_else(|| format!("Mod {} has no pack file name", item.id))?;
        if !pack_name.to_ascii_lowercase().ends_with(".pack") {
            return Err(format!(
                "Mod {} does not resolve to a .pack file: {}",
                item.id,
                path.display()
            ));
        }
        let mod_dir = path
            .parent()
            .ok_or_else(|| format!("Mod {} has no parent directory", item.id))?;
        if !same_manifest_path(mod_dir, &data_dir) {
            let key = manifest_path_key(mod_dir);
            if seen_dirs.insert(key) {
                working_dirs.push(format!(
                    "add_working_directory \"{}\";",
                    manifest_quote(&mod_dir.display().to_string())
                ));
            }
        }
        pack_lines.push(format!("mod \"{}\";", manifest_quote(pack_name)));
    }

    working_dirs.extend(pack_lines);
    Ok(working_dirs.join("\n"))
}

pub fn write_mods_manifest(
    config: &ModsManifestConfig,
    install_dir: &Path,
    content: String,
) -> Result<ManifestBuildResult, String> {
    let primary_path = install_dir.join(config.primary_file_name);
    match fs::write(&primary_path, content.as_bytes()) {
        Ok(()) => Ok(ManifestBuildResult {
            file_name: config.primary_file_name.to_string(),
            path: primary_path,
            content,
            written: true,
        }),
        Err(primary_err) => {
            let fallback_path = install_dir.join(config.fallback_file_name);
            fs::write(&fallback_path, content.as_bytes()).map_err(|fallback_err| {
                format!(
                    "Failed to write {}: {}; fallback {} also failed: {}",
                    primary_path.display(),
                    primary_err,
                    fallback_path.display(),
                    fallback_err
                )
            })?;
            Ok(ManifestBuildResult {
                file_name: config.fallback_file_name.to_string(),
                path: fallback_path,
                content,
                written: true,
            })
        }
    }
}

fn same_manifest_path(left: &Path, right: &Path) -> bool {
    manifest_path_key(left) == manifest_path_key(right)
}

fn manifest_path_key(path: &Path) -> String {
    let normalized = path
        .to_string_lossy()
        .replace('\\', "/")
        .trim_end_matches('/')
        .to_string();
    if cfg!(windows) {
        normalized.to_ascii_lowercase()
    } else {
        normalized
    }
}

fn manifest_quote(value: &str) -> String {
    value.replace('"', "\\\"")
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_MANIFEST: ModsManifestConfig = ModsManifestConfig {
        primary_file_name: "used_mods.txt",
        fallback_file_name: "my_mods.txt",
        encoding: ManifestEncoding::Utf8,
    };

    #[test]
    fn run_script_template_renders_mods_and_manifest_name() {
        let vars = RunScriptTemplateVars {
            mods: vec!["D:/Mods/a.pack".to_string(), "D:/Mods/b.pack".to_string()],
            mod_ids: vec!["a".to_string(), "b".to_string()],
            manifest_name: Some("used_mods.txt"),
            profile: Some("Campaign"),
            extra: vec!["-foo".to_string(), "bar".to_string()],
        };

        assert_eq!(
            render_run_script_template("{mods_sep=;} {manifest_name}; {profile} {extra}", &vars),
            "D:/Mods/a.pack;D:/Mods/b.pack used_mods.txt; Campaign -foo bar"
        );
    }

    #[test]
    fn mods_manifest_renders_exact_wh_style_bytes() {
        let dir = tempfile::tempdir().expect("temp dir");
        let install_dir = dir.path().join("Total War WARHAMMER III");
        let data_dir = install_dir.join("data");
        let workshop_a = dir.path().join("workshop").join("111");
        let workshop_b = dir.path().join("workshop").join("222");
        fs::create_dir_all(&data_dir).expect("data dir");
        fs::create_dir_all(&workshop_a).expect("workshop a");
        fs::create_dir_all(&workshop_b).expect("workshop b");
        let mods = vec![
            ResolvedMod {
                id: "111".to_string(),
                path: Some(workshop_a.join("alpha.pack").display().to_string()),
            },
            ResolvedMod {
                id: "222".to_string(),
                path: Some(workshop_b.join("beta.pack").display().to_string()),
            },
            ResolvedMod {
                id: "data-pack".to_string(),
                path: Some(data_dir.join("local.pack").display().to_string()),
            },
        ];

        let rendered = render_mods_manifest(&TEST_MANIFEST, &install_dir, &mods).expect("manifest");

        let expected = [
            format!("add_working_directory \"{}\";", workshop_a.display()),
            format!("add_working_directory \"{}\";", workshop_b.display()),
            "mod \"alpha.pack\";".to_string(),
            "mod \"beta.pack\";".to_string(),
            "mod \"local.pack\";".to_string(),
        ]
        .join("\n");
        assert_eq!(rendered, expected);
    }

    #[test]
    fn write_mods_manifest_writes_utf8_used_mods_file() {
        let dir = tempfile::tempdir().expect("temp dir");
        let content = "mod \"alpha.pack\";".to_string();

        let result = write_mods_manifest(&TEST_MANIFEST, dir.path(), content.clone())
            .expect("write manifest");

        assert_eq!(result.file_name, "used_mods.txt");
        assert!(result.written);
        assert_eq!(
            fs::read_to_string(dir.path().join("used_mods.txt")).expect("read manifest"),
            content
        );
    }
}
