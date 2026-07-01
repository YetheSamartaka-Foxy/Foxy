use super::{
    AppUpdateMode, DownloadSummary, DownloadTelemetrySample, Repository, RepositoryProfile,
    SettingsViewState, additional_folder_alias_key, apply_repo_client_parameters,
    apply_repo_dlc_content_from_repo_json, merge_remote_addon_list, normalize_loaded_repository,
    sanitize_external_addons, sanitize_settings_paths, selected_creator_dlc_codes,
    split_additional_launch_params,
};
use serde_json::json;
use std::time::Duration;

use crate::ui::theme::Theme;

#[test]
fn download_summary_planned_or_downloaded_falls_back_to_downloaded() {
    let summary = DownloadSummary {
        mods_updated: 1,
        files_updated: 1,
        parts_updated: 0,
        downloaded_bytes: 512,
        planned_transfer_bytes: 0,
        full_download_bytes: 0,
        patch_savings_bytes: 0,
        patched_files: 0,
        download_stage_duration: Duration::ZERO,
        cumulative_hash_duration: Duration::ZERO,
        after_download_hash_duration: Duration::ZERO,
        hash_stage_duration: Duration::ZERO,
        total_duration: Duration::ZERO,
        avg_speed_bps: 0.0,
        telemetry_samples: Vec::new(),
    };

    assert_eq!(summary.planned_or_downloaded_bytes(), 512);
    assert_eq!(
        summary.cumulative_or_after_download_hash_duration(),
        Duration::ZERO
    );
}

#[test]
fn download_summary_hash_duration_helpers_preserve_legacy_duration() {
    let summary = DownloadSummary {
        mods_updated: 1,
        files_updated: 1,
        parts_updated: 0,
        downloaded_bytes: 512,
        planned_transfer_bytes: 0,
        full_download_bytes: 0,
        patch_savings_bytes: 0,
        patched_files: 0,
        download_stage_duration: Duration::ZERO,
        cumulative_hash_duration: Duration::ZERO,
        after_download_hash_duration: Duration::ZERO,
        hash_stage_duration: Duration::from_secs(3),
        total_duration: Duration::ZERO,
        avg_speed_bps: 0.0,
        telemetry_samples: Vec::new(),
    };

    assert_eq!(
        summary.cumulative_or_after_download_hash_duration(),
        Duration::from_secs(3)
    );
    assert_eq!(
        summary.after_download_or_legacy_hash_duration(),
        Duration::from_secs(3)
    );
}

#[test]
fn download_summary_telemetry_samples_are_capped() {
    let mut summary = DownloadSummary {
        mods_updated: 1,
        files_updated: 1,
        parts_updated: 0,
        downloaded_bytes: 0,
        planned_transfer_bytes: 0,
        full_download_bytes: 0,
        patch_savings_bytes: 0,
        patched_files: 0,
        download_stage_duration: Duration::ZERO,
        cumulative_hash_duration: Duration::ZERO,
        after_download_hash_duration: Duration::ZERO,
        hash_stage_duration: Duration::ZERO,
        total_duration: Duration::ZERO,
        avg_speed_bps: 0.0,
        telemetry_samples: Vec::new(),
    };

    for elapsed_ms in 0..200 {
        summary.push_telemetry_sample(DownloadTelemetrySample {
            elapsed_ms,
            download_bps: 0.0,
            disk_write_bps: 0.0,
            hash_files_per_sec: 0.0,
            cpu_percent: 0.0,
            memory_bytes: 0,
        });
    }

    assert_eq!(summary.telemetry_samples.len(), 180);
    assert_eq!(summary.telemetry_samples[0].elapsed_ms, 20);
}

#[test]
fn normalize_loaded_repository_preserves_profile_external_addons() {
    let mut repo = Repository {
        addons: vec![("repo_mod".to_string(), true)],
        optional_addons: vec![("optional_mod".to_string(), false)],
        external_addons: Vec::new(),
        profiles: vec![RepositoryProfile {
            addons: vec![("repo_mod".to_string(), false)],
            optional_addons: vec![("optional_mod".to_string(), true)],
            external_addons: vec![("@ace".to_string(), true, "C:\\Mods\\ACE".to_string())],
            ..RepositoryProfile::default()
        }],
        ..Repository::default()
    };

    normalize_loaded_repository(&mut repo);

    assert_eq!(
        repo.profiles[0].external_addons,
        vec![("@ace".to_string(), true, "C:\\Mods\\ACE".to_string())]
    );
    assert_eq!(
        repo.profiles[0].addons,
        vec![("repo_mod".to_string(), false)]
    );
    assert_eq!(
        repo.profiles[0].optional_addons,
        vec![("optional_mod".to_string(), true)]
    );
}

#[test]
fn merge_remote_addon_list_preserves_local_enable_disable_state() {
    let remote = vec![
        ("@mod_a".to_string(), true),
        ("@mod_b".to_string(), false),
        ("@mod_c".to_string(), true),
    ];
    let local = vec![("@mod_a".to_string(), false), ("@mod_b".to_string(), true)];

    let merged = merge_remote_addon_list(remote, &local);

    assert_eq!(
        merged,
        vec![
            ("@mod_a".to_string(), false),
            ("@mod_b".to_string(), true),
            ("@mod_c".to_string(), true),
        ]
    );
}

#[test]
fn merge_remote_addon_list_matches_names_case_insensitively() {
    let remote = vec![("@Mod_A".to_string(), true)];
    let local = vec![("@MOD_a".to_string(), false)];

    let merged = merge_remote_addon_list(remote, &local);

    assert_eq!(merged, vec![("@Mod_A".to_string(), false)]);
}

#[test]
fn merge_remote_addon_list_drops_addons_no_longer_remote() {
    let remote = vec![("@kept".to_string(), true)];
    let local = vec![("@kept".to_string(), false), ("@gone".to_string(), true)];

    let merged = merge_remote_addon_list(remote, &local);

    assert_eq!(merged, vec![("@kept".to_string(), false)]);
}

#[test]
fn sanitize_external_addons_deduplicates_by_name_and_path() {
    let mut addons = vec![
        (" @ace ".to_string(), false, "C:\\Mods\\ACE\\".to_string()),
        ("@ACE".to_string(), true, "c:/mods/ace".to_string()),
        ("@rhs".to_string(), true, "D:\\Mods\\RHS".to_string()),
    ];

    sanitize_external_addons(&mut addons);

    assert_eq!(
        addons,
        vec![
            ("@ace".to_string(), true, "C:\\Mods\\ACE\\".to_string()),
            ("@rhs".to_string(), true, "D:\\Mods\\RHS".to_string()),
        ]
    );
}

#[test]
fn sanitize_repository_paths_deduplicates_addon_favorites() {
    let mut repo = Repository {
        optional_addon_favorites: vec![
            " @ace ".to_string(),
            "@ACE".to_string(),
            String::new(),
            "@rhs".to_string(),
        ],
        optional_addon_client_side: vec![
            " @sound ".to_string(),
            "@SOUND".to_string(),
            String::new(),
            "@ui".to_string(),
        ],
        external_addon_favorites: vec![
            " C:\\Mods\\ACE\\ ".to_string(),
            "c:/mods/ace".to_string(),
            String::new(),
            "D:\\Mods\\RHS".to_string(),
        ],
        external_addon_client_side: vec![
            " C:\\Mods\\Sound\\ ".to_string(),
            "c:/mods/sound".to_string(),
            String::new(),
            "D:\\Mods\\UI".to_string(),
        ],
        ..Repository::default()
    };

    super::sanitize_repository_paths(&mut repo);

    assert_eq!(
        repo.optional_addon_favorites,
        vec!["@ace".to_string(), "@rhs".to_string()]
    );
    assert_eq!(
        repo.external_addon_favorites,
        vec!["C:\\Mods\\ACE\\".to_string(), "D:\\Mods\\RHS".to_string()]
    );
    assert_eq!(
        repo.optional_addon_client_side,
        vec!["@sound".to_string(), "@ui".to_string()]
    );
    assert_eq!(
        repo.external_addon_client_side,
        vec!["C:\\Mods\\Sound\\".to_string(), "D:\\Mods\\UI".to_string()]
    );
}

#[test]
fn selected_creator_dlc_codes_follow_expected_order() {
    let repo = Repository {
        gm: true,
        spe: true,
        ws: true,
        ..Repository::default()
    };

    assert_eq!(selected_creator_dlc_codes(&repo), vec!["gm", "spe", "ws"]);
}

#[test]
fn split_additional_launch_params_keeps_quoted_segments_together() {
    let args = split_additional_launch_params(
        r#"-skipIntro "-profiles=C:\Arma 3 Profiles" '-name=Jane Doe'"#,
    );

    assert_eq!(
        args,
        vec![
            "-skipIntro".to_string(),
            "-profiles=C:\\Arma 3 Profiles".to_string(),
            "-name=Jane Doe".to_string()
        ]
    );
}

#[test]
fn apply_repo_client_parameters_splits_basic_and_additional_params() {
    let mut repo = Repository {
        no_splash: true,
        additional_params: "-window".to_string(),
        ..Repository::default()
    };

    apply_repo_client_parameters(
        &mut repo,
        r#"-skipIntro -world=empty "-profiles=C:\Arma 3 Profiles" -window"#,
    );

    assert!(repo.skip_intro);
    assert!(!repo.no_splash);
    assert!(repo.world_empty);
    assert!(!repo.load_mission_to_memory);
    assert!(!repo.enable_ht);
    assert!(!repo.huge_pages);
    assert!(!repo.no_logs);
    assert_eq!(
        repo.additional_params,
        "-profiles=C:\\Arma 3 Profiles -window"
    );
}

#[test]
fn apply_repo_client_parameters_clears_repo_launch_params_when_empty() {
    let mut repo = Repository {
        skip_intro: true,
        no_splash: true,
        world_empty: true,
        load_mission_to_memory: true,
        enable_ht: true,
        huge_pages: true,
        no_logs: true,
        additional_params: "-window".to_string(),
        ..Repository::default()
    };

    apply_repo_client_parameters(&mut repo, "   ");

    assert!(!repo.skip_intro);
    assert!(!repo.no_splash);
    assert!(!repo.world_empty);
    assert!(!repo.load_mission_to_memory);
    assert!(!repo.enable_ht);
    assert!(!repo.huge_pages);
    assert!(!repo.no_logs);
    assert!(repo.additional_params.is_empty());
}

#[test]
fn apply_repo_dlc_content_from_repo_json_object_is_authoritative() {
    let mut repo = Repository {
        csla: true,
        ef: true,
        gm: true,
        rf: true,
        spe: true,
        vn: true,
        ws: true,
        ..Repository::default()
    };

    apply_repo_dlc_content_from_repo_json(
        &mut repo,
        &json!({
            "gm": true,
            "spe": true,
            "vn": false
        }),
    );

    assert!(!repo.csla);
    assert!(!repo.ef);
    assert!(repo.gm);
    assert!(!repo.rf);
    assert!(repo.spe);
    assert!(!repo.vn);
    assert!(!repo.ws);
}

#[test]
fn apply_repo_dlc_content_from_repo_json_array_supports_codes() {
    let mut repo = Repository::default();

    apply_repo_dlc_content_from_repo_json(&mut repo, &json!(["GM", "spe", "unknown", 42, " ws "]));

    assert!(!repo.csla);
    assert!(!repo.ef);
    assert!(repo.gm);
    assert!(!repo.rf);
    assert!(repo.spe);
    assert!(!repo.vn);
    assert!(repo.ws);
}

#[test]
fn sanitize_settings_paths_keeps_only_valid_additional_folder_aliases() {
    let mut settings = SettingsViewState {
        additional_folders: vec![
            " C:\\Mods\\AlphaPack ".to_string(),
            "D:/Mods/Bravo/".to_string(),
        ],
        ..Default::default()
    };
    settings
        .additional_folder_aliases
        .insert("C:\\Mods\\AlphaPack".to_string(), "  Alpha  ".to_string());
    settings
        .additional_folder_aliases
        .insert("d:/mods/bravo".to_string(), "  ".to_string());
    settings
        .additional_folder_aliases
        .insert("X:/Missing".to_string(), "Ghost".to_string());

    sanitize_settings_paths(&mut settings);

    assert_eq!(settings.additional_folder_aliases.len(), 1);
    assert_eq!(
        settings
            .additional_folder_aliases
            .get(&additional_folder_alias_key("C:\\Mods\\AlphaPack")),
        Some(&"Alpha".to_string())
    );
}

#[test]
fn sanitize_settings_paths_trims_arma3_profiles_directory() {
    let mut settings = SettingsViewState {
        arma3_profiles_directory: " D:\\Arma Profiles\\ ".to_string(),
        ..Default::default()
    };

    sanitize_settings_paths(&mut settings);

    assert_eq!(settings.arma3_profiles_directory, "D:\\Arma Profiles\\");
}

#[test]
fn settings_persist_saved_themes_but_not_editor_selection() {
    let mut settings = SettingsViewState::default();
    settings.saved_themes.push(Theme::from_current(
        "Field Theme",
        settings.font_sizes.clone(),
        settings.palette_colors.clone(),
    ));
    settings.selected_saved_theme = Some(0);
    settings.saved_theme_name_draft = "Unsaved rename".to_string();
    settings.show_add_theme_modal = true;
    settings.new_theme_name_draft = "Unsaved new theme".to_string();
    settings.focus_new_theme_name = true;

    let json = serde_json::to_string(&settings).expect("serialize settings");
    let restored: SettingsViewState = serde_json::from_str(&json).expect("deserialize settings");

    assert_eq!(restored.saved_themes, settings.saved_themes);
    assert_eq!(restored.selected_saved_theme, None);
    assert!(restored.saved_theme_name_draft.is_empty());
    assert!(!restored.show_add_theme_modal);
    assert!(restored.new_theme_name_draft.is_empty());
    assert!(!restored.focus_new_theme_name);
}

#[test]
fn settings_without_saved_themes_remain_compatible() {
    let mut value = serde_json::to_value(SettingsViewState::default()).expect("serialize settings");
    value
        .as_object_mut()
        .expect("settings object")
        .remove("saved_themes");

    let restored: SettingsViewState = serde_json::from_value(value).expect("deserialize settings");

    assert!(restored.saved_themes.is_empty());
}

#[test]
fn settings_default_to_github_app_updates() {
    let settings = SettingsViewState::default();

    assert_eq!(settings.app_update_mode, AppUpdateMode::GitHub);
    assert_eq!(settings.app_update_github_repo, "YetheSamartaka-Foxy/Foxy");
    assert!(!settings.app_update_mode_user_override);
}

#[test]
fn legacy_settings_without_app_update_mode_keep_server_url_source() {
    let mut value = serde_json::to_value(SettingsViewState::default()).expect("serialize settings");
    let object = value.as_object_mut().expect("settings object");
    object.remove("app_update_mode");
    object.remove("app_update_github_repo");
    object.remove("app_update_mode_user_override");
    object.insert(
        "app_update_url".to_string(),
        json!("https://updates.example.com/foxy/"),
    );
    object.insert("app_update_url_user_override".to_string(), json!(true));
    let settings: SettingsViewState = serde_json::from_value(value).expect("deserialize settings");

    assert_eq!(settings.app_update_mode, AppUpdateMode::Server);
    assert_eq!(settings.app_update_url, "https://updates.example.com/foxy/");
    assert_eq!(settings.app_update_github_repo, "YetheSamartaka-Foxy/Foxy");
    assert!(!settings.app_update_mode_user_override);
}

#[test]
fn settings_restore_explicit_github_update_mode() {
    let mut value = serde_json::to_value(SettingsViewState::default()).expect("serialize settings");
    let object = value.as_object_mut().expect("settings object");
    object.insert("app_update_mode".to_string(), json!("GitHub"));
    object.insert(
        "app_update_github_repo".to_string(),
        json!("YetheSamartaka-Foxy/Foxy"),
    );
    object.insert("app_update_mode_user_override".to_string(), json!(true));
    object.insert(
        "app_update_url".to_string(),
        json!("https://updates.example.com/foxy/"),
    );
    let settings: SettingsViewState = serde_json::from_value(value).expect("deserialize settings");

    assert_eq!(settings.app_update_mode, AppUpdateMode::GitHub);
    assert!(settings.app_update_mode_user_override);
    assert_eq!(settings.app_update_url, "https://updates.example.com/foxy/");
}

// ── split_additional_launch_params: additional ─────────────────────

#[test]
fn split_additional_launch_params_empty_string() {
    assert!(split_additional_launch_params("").is_empty());
}

#[test]
fn split_additional_launch_params_whitespace_only() {
    assert!(split_additional_launch_params("   ").is_empty());
}

#[test]
fn split_additional_launch_params_single_arg() {
    assert_eq!(
        split_additional_launch_params("-skipIntro"),
        vec!["-skipIntro"]
    );
}

#[test]
fn split_additional_launch_params_multiple_spaces() {
    let args = split_additional_launch_params("-a    -b    -c");
    assert_eq!(args, vec!["-a", "-b", "-c"]);
}

// ── selected_creator_dlc_codes: additional ─────────────────────────

#[test]
fn selected_creator_dlc_codes_none_selected() {
    let repo = Repository::default();
    assert!(selected_creator_dlc_codes(&repo).is_empty());
}

#[test]
fn selected_creator_dlc_codes_all_selected() {
    let repo = Repository {
        csla: true,
        ef: true,
        gm: true,
        rf: true,
        spe: true,
        vn: true,
        ws: true,
        ..Repository::default()
    };
    let codes = selected_creator_dlc_codes(&repo);
    assert_eq!(codes.len(), 7);
    assert!(codes.contains(&"csla"));
    assert!(codes.contains(&"ef"));
    assert!(codes.contains(&"gm"));
    assert!(codes.contains(&"rf"));
    assert!(codes.contains(&"spe"));
    assert!(codes.contains(&"vn"));
    assert!(codes.contains(&"ws"));
}

// ── apply_repo_dlc_content_from_repo_json: additional ──────────────

#[test]
fn apply_repo_dlc_content_from_repo_json_non_object_non_array_is_noop() {
    let mut repo = Repository {
        gm: true,
        ..Repository::default()
    };
    apply_repo_dlc_content_from_repo_json(&mut repo, &json!("invalid"));
    // Should not change anything
    assert!(repo.gm);
}

#[test]
fn apply_repo_dlc_content_from_repo_json_null_is_noop() {
    let mut repo = Repository {
        spe: true,
        ..Repository::default()
    };
    apply_repo_dlc_content_from_repo_json(&mut repo, &json!(null));
    assert!(repo.spe);
}

#[test]
fn apply_repo_dlc_content_from_repo_json_empty_object_disables_all() {
    let mut repo = Repository {
        csla: true,
        ef: true,
        gm: true,
        rf: true,
        spe: true,
        vn: true,
        ws: true,
        ..Repository::default()
    };
    apply_repo_dlc_content_from_repo_json(&mut repo, &json!({}));
    assert!(!repo.csla);
    assert!(!repo.ef);
    assert!(!repo.gm);
    assert!(!repo.rf);
    assert!(!repo.spe);
    assert!(!repo.vn);
    assert!(!repo.ws);
}

#[test]
fn apply_repo_dlc_content_from_repo_json_empty_array_disables_all() {
    let mut repo = Repository {
        gm: true,
        spe: true,
        ..Repository::default()
    };
    apply_repo_dlc_content_from_repo_json(&mut repo, &json!([]));
    assert!(!repo.gm);
    assert!(!repo.spe);
}

// ── sanitize_external_addons: additional ───────────────────────────

#[test]
fn sanitize_external_addons_empty_name_removed() {
    let mut addons = vec![("  ".to_string(), true, "C:\\Mods".to_string())];
    sanitize_external_addons(&mut addons);
    assert!(addons.is_empty());
}

#[test]
fn sanitize_external_addons_empty_vec() {
    let mut addons: Vec<(String, bool, String)> = vec![];
    sanitize_external_addons(&mut addons);
    assert!(addons.is_empty());
}

// ── apply_repo_client_parameters: additional ───────────────────────

#[test]
fn apply_repo_client_parameters_all_known_flags() {
    let mut repo = Repository::default();
    apply_repo_client_parameters(
        &mut repo,
        "-skipIntro -noSplash -world=empty -loadMissionToMemory -enableHT -hugePages -noLogs",
    );
    assert!(repo.skip_intro);
    assert!(repo.no_splash);
    assert!(repo.world_empty);
    assert!(repo.load_mission_to_memory);
    assert!(repo.enable_ht);
    assert!(repo.huge_pages);
    assert!(repo.no_logs);
    assert!(repo.additional_params.is_empty());
}

#[test]
fn apply_repo_client_parameters_case_insensitive() {
    let mut repo = Repository::default();
    apply_repo_client_parameters(&mut repo, "-SKIPINTRO -NOSPLASH");
    assert!(repo.skip_intro);
    assert!(repo.no_splash);
}
