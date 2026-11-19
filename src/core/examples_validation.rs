use std::collections::BTreeMap;

use serde::de::DeserializeOwned;

use crate::core::game::reforger::ReforgerAddonsFile;
use crate::core::game::spaces::GamesRegistryFile;
use crate::core::game::workshop::{WorkshopFile, bundle::BundleManifest};
use crate::core::server::{
    ConsumerRecoveryPlan, ContentSources, PortableInstancePack, ReforgerScenarioEnd,
    ServerRestoreOutcome,
};
use crate::ui::palette::{contrast_ratio, contrast_text_on};
use crate::ui::types::{
    ActiveUpdateSession, Repository, RepositorySpace, RepositoryVisualFolder, SettingsViewState,
    WindowState,
};

fn parse_example<T: DeserializeOwned>(name: &str, raw: &str) -> T {
    serde_json::from_str(raw).unwrap_or_else(|err| panic!("failed to parse {name}: {err}"))
}

#[test]
fn all_client_and_appdata_examples_deserialize_and_meet_wcag_palette() {
    let _session: ActiveUpdateSession = parse_example(
        "active_update_session.json",
        include_str!("../../examples/json/active_update_session.json"),
    );
    let share: BundleManifest = parse_example(
        "foxyshare_manifest.json",
        include_str!("../../examples/json/foxyshare_manifest.json"),
    );
    assert!(share.schema_version >= 1);

    let _window: WindowState = parse_example(
        "window_state.json",
        include_str!("../../examples/json/appdata/window_state.json"),
    );
    let games: GamesRegistryFile = parse_example(
        "games.json",
        include_str!("../../examples/json/appdata/games.json"),
    );
    assert!(!games.game_spaces.is_empty());

    let defaults = serde_json::to_value(SettingsViewState::default()).expect("default settings");
    let app_value: serde_json::Value = parse_example(
        "app_settings.json",
        include_str!("../../examples/json/appdata/app_settings.json"),
    );
    let mut merged_app = defaults.as_object().cloned().expect("defaults object");
    for (k, v) in app_value.as_object().expect("app_settings object") {
        merged_app.insert(k.clone(), v.clone());
    }
    let app_settings: SettingsViewState =
        serde_json::from_value(serde_json::Value::Object(merged_app))
            .expect("parse merged app_settings.json");

    let palette = &app_settings.palette_colors;
    for (label, fg, bg) in [
        (
            "text_normal/main_bg",
            palette.text_normal.to_color32(),
            palette.main_bg.to_color32(),
        ),
        (
            "text_gray/main_bg",
            palette.text_gray.to_color32(),
            palette.main_bg.to_color32(),
        ),
        (
            "text_dim/card_bg",
            palette.text_dim.to_color32(),
            palette.card_bg.to_color32(),
        ),
        (
            "text_error/widget_bg",
            palette.text_error.to_color32(),
            palette.widget_bg.to_color32(),
        ),
        (
            "debug/main_bg",
            palette.debug.to_color32(),
            palette.main_bg.to_color32(),
        ),
        (
            "success/main_bg",
            palette.success.to_color32(),
            palette.main_bg.to_color32(),
        ),
        (
            "on_action_info/action_info",
            contrast_text_on(palette.action_info.to_color32()),
            palette.action_info.to_color32(),
        ),
        (
            "on_destructive/action_destructive",
            contrast_text_on(palette.action_destructive.to_color32()),
            palette.action_destructive.to_color32(),
        ),
    ] {
        let ratio = contrast_ratio(fg, bg);
        assert!(
            ratio >= 4.5,
            "example app_settings.json `{label}` contrast {ratio:.2}:1 below WCAG AA 4.5:1"
        );
    }

    for (name, raw) in [
        (
            "arma3/repositories.json",
            include_str!("../../examples/json/appdata/games/arma3/repositories.json"),
        ),
        (
            "twwh3/repositories.json",
            include_str!("../../examples/json/appdata/games/twwh3/repositories.json"),
        ),
        (
            "reforger/repositories.json",
            include_str!("../../examples/json/appdata/games/reforger/repositories.json"),
        ),
        (
            "generic/repositories.json",
            include_str!("../../examples/json/appdata/games/generic/repositories.json"),
        ),
    ] {
        let _: Vec<Repository> = parse_example(name, raw);
    }

    for (name, raw) in [
        (
            "arma3/repository_spaces.json",
            include_str!("../../examples/json/appdata/games/arma3/repository_spaces.json"),
        ),
        (
            "twwh3/repository_spaces.json",
            include_str!("../../examples/json/appdata/games/twwh3/repository_spaces.json"),
        ),
        (
            "reforger/repository_spaces.json",
            include_str!("../../examples/json/appdata/games/reforger/repository_spaces.json"),
        ),
        (
            "generic/repository_spaces.json",
            include_str!("../../examples/json/appdata/games/generic/repository_spaces.json"),
        ),
    ] {
        let _: Vec<RepositorySpace> = parse_example(name, raw);
    }

    let _: Vec<RepositoryVisualFolder> = parse_example(
        "arma3/repository_visual_folders.json",
        include_str!("../../examples/json/appdata/games/arma3/repository_visual_folders.json"),
    );

    for (name, raw) in [
        (
            "arma3/workshop.json",
            include_str!("../../examples/json/appdata/games/arma3/workshop.json"),
        ),
        (
            "twwh3/workshop.json",
            include_str!("../../examples/json/appdata/games/twwh3/workshop.json"),
        ),
        (
            "reforger/workshop.json",
            include_str!("../../examples/json/appdata/games/reforger/workshop.json"),
        ),
        (
            "generic/workshop.json",
            include_str!("../../examples/json/appdata/games/generic/workshop.json"),
        ),
    ] {
        let _: WorkshopFile = parse_example(name, raw);
    }

    let _: ReforgerAddonsFile = parse_example(
        "reforger/reforger_addons.json",
        include_str!("../../examples/json/appdata/games/reforger/reforger_addons.json"),
    );
}

#[test]
fn remaining_server_backend_examples_deserialize_and_contain_no_em_dashes() {
    let _: ContentSources = parse_example(
        "arma3/server/content-sources.json",
        include_str!("../../examples/json/appdata/games/arma3/server/content-sources.json"),
    );
    let _: ContentSources = parse_example(
        "reforger/server/content-sources.json",
        include_str!("../../examples/json/appdata/games/reforger/server/content-sources.json"),
    );
    let _: PortableInstancePack = parse_example(
        "arma3_instance_pack.json",
        include_str!("../../examples/json/server_backend/arma3_instance_pack.json"),
    );
    let scenario_end: BTreeMap<String, ReforgerScenarioEnd> = parse_example(
        "reforger_scenario_end.json",
        include_str!("../../examples/json/server_backend/reforger_scenario_end.json"),
    );
    assert!(!scenario_end.is_empty());

    let _: foxy_supervisor::recovery::ConsumerRecord = parse_example(
        "server_consumer_registration.json",
        include_str!("../../examples/json/server_backend/server_consumer_registration.json"),
    );
    let _: ConsumerRecoveryPlan = parse_example(
        "server_consumer_recovery_plan.json",
        include_str!("../../examples/json/server_backend/server_consumer_recovery_plan.json"),
    );
    let _: foxy_supervisor::recovery::witness::CleanupReceipt = parse_example(
        "server_consumer_cleanup_receipt.json",
        include_str!("../../examples/json/server_backend/server_consumer_cleanup_receipt.json"),
    );
    let _: foxy_supervisor::maintenance::MaintenanceIntent = parse_example(
        "server_maintenance_intent.json",
        include_str!("../../examples/json/server_backend/server_maintenance_intent.json"),
    );
    let _: foxy_supervisor::maintenance::MaintenanceReceipt = parse_example(
        "server_maintenance_receipt.json",
        include_str!("../../examples/json/server_backend/server_maintenance_receipt.json"),
    );
    let _: ServerRestoreOutcome = parse_example(
        "server_restore_outcome.json",
        include_str!("../../examples/json/server_backend/server_restore_outcome.json"),
    );

    for raw in [
        include_str!("../../examples/json/appdata/app_settings.json"),
        include_str!("../../examples/json/appdata/games.json"),
        include_str!("../../examples/json/server_backend/arma3_instance_pack.json"),
        include_str!("../../examples/json/server_backend/server_reforger_maintenance_jobs.json"),
    ] {
        assert!(!raw.contains('\u{2014}') && !raw.contains('\u{2013}'));
    }
}
