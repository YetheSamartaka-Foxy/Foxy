use std::collections::{BTreeSet, HashMap};

use eframe::egui;
use eframe::egui::accesskit::{Node, NodeId, Role};

use crate::ui::app::Foxy;

mod rtl;

fn node_text_from_tree(nodes: &HashMap<NodeId, Node>, id: NodeId, depth: u8) -> Option<String> {
    if depth > 4 {
        return None;
    }
    let node = nodes.get(&id)?;
    if let Some(label) = node.label() {
        let trimmed = label.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }
    if let Some(value) = node.value() {
        let trimmed = value.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }
    for child in node.children() {
        if let Some(text) = node_text_from_tree(nodes, *child, depth + 1) {
            return Some(text);
        }
    }
    None
}

fn resolved_accessible_name(nodes: &HashMap<NodeId, Node>, node: &Node) -> Option<String> {
    if let Some(label) = node.label() {
        let trimmed = label.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }
    for label_id in node.labelled_by() {
        if let Some(text) = node_text_from_tree(nodes, *label_id, 0) {
            return Some(text);
        }
    }
    None
}

fn is_meaningless_symbol_name(name: &str) -> bool {
    let trimmed = name.trim();
    trimmed.is_empty()
        || matches!(
            trimmed,
            "X" | "\u{00D7}" | "\u{2699}" | "\u{21bb}" | "+" | "-" | "?" | "..."
        )
        || trimmed.chars().all(|c| !c.is_alphanumeric())
}

fn run_accesskit_audit(
    screen: &str,
    wrap_in_central_panel: bool,
    mut setup: impl FnMut(&mut Foxy),
    mut draw: impl FnMut(&mut Foxy, &mut egui::Ui),
) -> Vec<String> {
    let ctx = egui::Context::default();
    ctx.enable_accesskit();
    let mut app = Foxy::new_headless(&ctx);
    setup(&mut app);

    let mut nodes: HashMap<NodeId, Node> = HashMap::new();
    let viewport = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1280.0, 900.0));
    for _ in 0..2 {
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(viewport),
                ..Default::default()
            },
            |ui| {
                if wrap_in_central_panel {
                    egui::CentralPanel::default().show(ui, |ui| {
                        draw(&mut app, ui);
                    });
                } else {
                    draw(&mut app, ui);
                }
            },
        );
        if let Some(update) = output.platform_output.accesskit_update.take() {
            for (id, node) in update.nodes {
                nodes.insert(id, node);
            }
        }
        output.textures_delta.clear();
    }

    let mut issues = Vec::new();
    let mut seen = BTreeSet::new();
    for (id, node) in &nodes {
        let role = node.role();
        if node.is_hidden() || !egui::accessibility::is_input(role) {
            continue;
        }
        if matches!(role, Role::Splitter) {
            continue;
        }
        let resolved = resolved_accessible_name(&nodes, node);
        let bad = match resolved.as_deref() {
            None => Some("missing accessible name / label".to_string()),
            Some(name) if is_meaningless_symbol_name(name) => {
                Some(format!("non-descriptive symbol name `{name}`"))
            }
            Some("Optional") => {
                Some("generic `Optional` hint exposed instead of field label".to_string())
            }
            Some(_) => None,
        };
        if let Some(reason) = bad {
            let entry = format!(
                "{screen}: role={role:?} id={id:?} value={:?} -> {reason}",
                node.value()
            );
            if seen.insert(entry.clone()) {
                issues.push(entry);
            }
        }
    }
    issues
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use chrono::Local;

    use super::*;
    use crate::core::benchmarks::{
        BenchmarkBuild, BenchmarkKind, BenchmarkMachine, BenchmarkMetrics, BenchmarkOutcome,
        BenchmarkRecord, BenchmarkRepository,
    };
    use crate::core::server::jobs::{MaintenanceOverview, MaintenancePolicy};
    use crate::core::server::owner::OwnerLifetime;
    use crate::core::server::{ServerCatalog, ServerInstanceId};
    use crate::ui::app::server::{
        MaintenanceForm, MaintenanceSnapshot, RconForm, RconSnapshot, ServerDialog, ServerPage,
    };
    use crate::ui::types::{
        AboutTab, HelpTab, JobSchedule, JobTarget, PostAction, ProxyMode, RepoInstanceKey,
        Repository, RepositorySettingsTab, RepositorySpace, ScheduleJobDraft, ScheduleKind,
        ScheduledJob, WEEKDAYS_ALL,
    };

    fn seed_repository(app: &mut Foxy) {
        let mut repo = Repository {
            name: "MainRepo".to_string(),
            address: "https://repo.example.invalid/main/".to_string(),
            path: "R:/Mods/MainRepo".to_string(),
            ..Default::default()
        };
        repo.addons.push(("@mod_01".to_string(), true));
        repo.optional_addons.push(("@opt_01".to_string(), false));
        repo.external_addons
            .push(("@ext_01".to_string(), true, "R:/Mods/Ext".to_string()));
        app.repository_view_state.repositories = vec![repo];
        app.repository_view_state.selected_repository = Some(0);
        app.selected_repository_for_settings = Some(0);
    }

    fn seed_benchmark_record() -> BenchmarkRecord {
        BenchmarkRecord {
            id: "bench-audit-01".to_string(),
            record_version: crate::core::benchmarks::BENCHMARK_RECORD_VERSION,
            name: "Recheck MainRepo".to_string(),
            notes: String::new(),
            kind: BenchmarkKind::Recheck,
            started_at: 1_700_000_000,
            started_at_local: "2026-01-01 12:00:00".to_string(),
            finished_at_local: "2026-01-01 12:00:05".to_string(),
            elapsed_ms: 5_000,
            outcome: BenchmarkOutcome::Success,
            operation_id: None,
            repository: BenchmarkRepository {
                name: "MainRepo".to_string(),
                url: "https://repo.example.invalid/main/".to_string(),
                local_path: "R:/Mods/MainRepo".to_string(),
                space_id: Some("arma3".to_string()),
            },
            addons: vec!["@mod_01".to_string()],
            favourite: false,
            hidden: false,
            build: BenchmarkBuild::default(),
            machine: BenchmarkMachine::default(),
            metrics: BenchmarkMetrics::default(),
            stages: Vec::new(),
            sol: Vec::new(),
            samples: Vec::new(),
            stage_marks: Vec::new(),
            log_file: None,
            log_line_count: 0,
        }
    }

    fn seed_scheduled_job() -> ScheduledJob {
        ScheduledJob {
            id: "job-audit-01".to_string(),
            name: "Nightly check".to_string(),
            enabled: true,
            schedule: JobSchedule::Recurring {
                minutes_of_day: 240,
                weekdays: WEEKDAYS_ALL,
            },
            target: JobTarget::Repository(RepoInstanceKey {
                remote_url: "https://repo.example.invalid/main/".to_string(),
                local_path: "R:/Mods/MainRepo".to_string(),
            }),
            recheck: true,
            download: false,
            post_action: PostAction::None,
            post_action_only_on_success: true,
            last_run_unix_ms: None,
            last_result: None,
        }
    }

    fn seed_server_catalog(app: &mut Foxy, catalog_json: &str) -> ServerInstanceId {
        let catalog: ServerCatalog =
            serde_json::from_str(catalog_json).expect("parse example servers.json");
        let first_id = catalog.instances[0].id.clone();
        app.server_view.catalog = Some(catalog);
        app.server_view.loaded = true;
        assert!(app.server_view.select(&first_id));
        first_id
    }

    #[test]
    fn all_shipped_views_expose_descriptive_accesskit_input_names() {
        const AUDIT_CHILD: &str = "FOXY_ACCESSKIT_AUDIT_CHILD";
        if std::env::var_os(AUDIT_CHILD).is_none() {
            let root = tempfile::tempdir().expect("isolated UI configuration");
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "ui::views::accesskit_audit::tests::all_shipped_views_expose_descriptive_accesskit_input_names",
                    "--nocapture",
                ])
                .env("FOXY_CONFIG_DIR", root.path())
                .env(AUDIT_CHILD, "1")
                .output()
                .expect("run the AccessKit audit in isolation");
            let stdout = String::from_utf8_lossy(&output.stdout);
            assert!(
                output.status.success() && stdout.contains("1 passed; 0 failed"),
                "{}\n{}",
                stdout,
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }

        let mut all_issues = Vec::new();

        all_issues.extend(run_accesskit_audit(
            "app_header",
            true,
            |_| {},
            |app, ui| app.render_app_header(ui),
        ));

        all_issues.extend(run_accesskit_audit(
            "main_chrome",
            false,
            |_| {},
            |app, ui| app.render_main_contents(ui),
        ));

        for tab in HelpTab::all_tabs() {
            all_issues.extend(run_accesskit_audit(
                &format!("help::{tab:?}"),
                true,
                move |app| app.current_help_tab = tab,
                |app, ui| app.render_help_contents(ui),
            ));
        }

        for tab in AboutTab::all_tabs() {
            all_issues.extend(run_accesskit_audit(
                &format!("about::{tab:?}"),
                true,
                move |app| app.current_about_tab = tab,
                |app, ui| app.render_about_contents(ui),
            ));
        }

        all_issues.extend(run_accesskit_audit(
            "changelog",
            true,
            |_| {},
            |app, ui| app.render_changelog_contents(ui),
        ));

        all_issues.extend(run_accesskit_audit(
            "game_spaces",
            true,
            |app| {
                app.game_spaces_view_state.show_create_modal = true;
            },
            |app, ui| app.render_game_spaces_contents(ui),
        ));

        all_issues.extend(run_accesskit_audit(
            "game_space_settings",
            true,
            |_| {},
            |app, ui| app.render_game_space_settings_contents(ui),
        ));

        for tab in [
            "Application",
            "Scheduling",
            "Backup Manager",
            "Cleanup",
            "Direct download",
            "Customization",
            "Benchmarks",
        ] {
            all_issues.extend(run_accesskit_audit(
                &format!("settings::{tab}"),
                true,
                move |app| {
                    seed_repository(app);
                    app.settings_view_state.current_tab = tab.to_string();
                    app.settings_view_state.proxy_mode = ProxyMode::Manual;
                    app.settings_view_state.scheduled_jobs = vec![seed_scheduled_job()];
                    app.benchmarks_view.loaded = true;
                    app.benchmarks_view.records = vec![seed_benchmark_record()];
                },
                |app, ui| app.render_settings_contents(ui),
            ));
        }

        for (label, kind) in [
            ("settings::Scheduling::editor_once", ScheduleKind::Once),
            (
                "settings::Scheduling::editor_recurring",
                ScheduleKind::Recurring,
            ),
        ] {
            all_issues.extend(run_accesskit_audit(
                label,
                true,
                move |app| {
                    seed_repository(app);
                    app.settings_view_state.current_tab = "Scheduling".to_string();
                    let mut draft = ScheduleJobDraft::new_add(Local::now());
                    draft.kind = kind;
                    app.scheduling_editor = Some(draft);
                },
                |app, ui| app.render_settings_contents(ui),
            ));
        }

        all_issues.extend(run_accesskit_audit(
            "repository_view",
            false,
            |app| {
                seed_repository(app);
                app.show_add_repository_modal = true;
            },
            |app, ui| app.render_repository_contents(ui),
        ));

        all_issues.extend(run_accesskit_audit(
            "repository_space_settings",
            true,
            |app| {
                let spaces: Vec<RepositorySpace> = serde_json::from_str(include_str!(
                    "../../../examples/json/appdata/games/arma3/repository_spaces.json"
                ))
                .expect("parse example repository_spaces.json");
                let id = spaces[0].id.clone();
                app.repository_spaces = spaces;
                app.selected_repository_space_id = Some(id.clone());
                app.open_repository_space_settings(&id);
            },
            |app, ui| app.render_repository_space_settings_contents(ui),
        ));

        for tab in RepositorySettingsTab::all_tabs() {
            all_issues.extend(run_accesskit_audit(
                &format!("repository_settings::{tab:?}"),
                true,
                move |app| {
                    seed_repository(app);
                    app.current_repository_settings_tab = tab;
                },
                |app, ui| app.render_repository_settings_contents(ui),
            ));
        }

        all_issues.extend(run_accesskit_audit(
            "workshop",
            true,
            |app| {
                app.workshop_view_state.show_import_modal = true;
            },
            |app, ui| {
                app.render_workshop_tab(ui, true);
                app.render_workshop_modals(ui);
            },
        ));

        all_issues.extend(run_accesskit_audit(
            "swifty_migration",
            true,
            |_| {},
            |app, ui| app.render_swifty_migration_contents(ui),
        ));

        for (label, catalog_json, reforger) in [
            (
                "server::arma3",
                include_str!("../../../examples/json/appdata/games/arma3/server/servers.json"),
                false,
            ),
            (
                "server::reforger",
                include_str!("../../../examples/json/appdata/games/reforger/server/servers.json"),
                true,
            ),
        ] {
            for page in [
                ServerPage::Overview,
                ServerPage::Setup,
                ServerPage::Installation,
                ServerPage::Import,
                ServerPage::Configuration,
                ServerPage::Content,
                ServerPage::Addons,
                ServerPage::Profiles,
                ServerPage::Records,
                ServerPage::Deployment,
                ServerPage::Operations,
                ServerPage::Recovery,
                ServerPage::Tools,
            ] {
                all_issues.extend(run_accesskit_audit(
                    &format!("{label}::{page:?}"),
                    true,
                    |app| {
                        seed_server_catalog(app, catalog_json);
                        app.server_view.page = page;
                    },
                    |app, ui| app.render_server_workspace(ui),
                ));
            }
            all_issues.extend(run_accesskit_audit(
                label,
                true,
                move |app| {
                    let instance_id = seed_server_catalog(app, catalog_json);
                    app.server_view.rcon.last_refresh = Some(Instant::now());
                    let rcon_snapshot = RconSnapshot {
                        instance_id: instance_id.clone(),
                        draft_revision: 1,
                        reforger,
                        draft: None,
                        password_available: false,
                        run_generation: None,
                        session: None,
                        session_note: None,
                        recent: Vec::new(),
                        unreadable: 0,
                    };
                    app.server_view.rcon.form = Some(RconForm::open(&rcon_snapshot));
                    app.server_view.rcon.snapshot = Some(rcon_snapshot);

                    app.server_view.maintenance.last_refresh = Some(Instant::now());
                    app.server_view.maintenance.snapshot = Some(MaintenanceSnapshot {
                        instance_id,
                        overview: MaintenanceOverview {
                            catalog_revision: 1,
                            owner_running: false,
                            jobs: Vec::new(),
                            unreadable_occurrences: 0,
                            history_truncated: false,
                        },
                        recent: Vec::new(),
                    });
                    app.server_view.maintenance.form = Some(MaintenanceForm::new_add(Local::now()));
                    app.server_view.open_dialog(ServerDialog::Rename);
                    app.server_view.dialog_name = "Audit server".to_string();
                },
                |app, ui| {
                    app.render_server_overview(ui);
                    app.render_server_editor(ui);
                    app.render_server_maintenance(ui);
                    app.render_server_rcon(ui);
                    app.render_server_dialog(ui.ctx());
                },
            ));
        }

        all_issues.extend(run_accesskit_audit(
            "server::add",
            true,
            |app| {
                app.server_view.loaded = true;
                app.server_view.page = ServerPage::Add;
            },
            |app, ui| app.render_server_workspace(ui),
        ));

        // Verify Server dialog variants outside active RCon/maintenance forms too.
        for dialog in [
            ServerDialog::Remove,
            ServerDialog::RestoreServerBackup,
            ServerDialog::AcquireRepositoryContent,
        ] {
            all_issues.extend(run_accesskit_audit(
                &format!("server::dialog::{dialog:?}"),
                true,
                move |app| {
                    seed_server_catalog(
                        app,
                        include_str!(
                            "../../../examples/json/appdata/games/arma3/server/servers.json"
                        ),
                    );
                    app.server_view.open_dialog(dialog);
                    app.server_view.dialog_name = "Audit server".to_string();
                },
                |app, ui| app.render_server_dialog(ui.ctx()),
            ));
        }
        let _ = (OwnerLifetime::Foreground, MaintenancePolicy::default());

        assert!(
            all_issues.is_empty(),
            "AccessKit input audit failures ({}):\n{}",
            all_issues.len(),
            all_issues.join("\n")
        );
    }

    #[test]
    fn server_draft_removal_initially_focuses_cancel() {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let mut app = Foxy::new_headless(&ctx);
        seed_server_catalog(
            &mut app,
            include_str!("../../../examples/json/appdata/games/arma3/server/servers.json"),
        );
        app.server_view.open_dialog(ServerDialog::Remove);

        let viewport = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0));
        let mut nodes = HashMap::new();
        let mut focus = None;
        for _ in 0..2 {
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(viewport),
                    ..Default::default()
                },
                |ui| app.render_server_dialog(ui.ctx()),
            );
            output.textures_delta.clear();
            let tree = output
                .platform_output
                .accesskit_update
                .expect("dialog tree");
            focus = Some(tree.focus);
            nodes.extend(tree.nodes);
        }
        let name = focus
            .and_then(|id| nodes.get(&id))
            .and_then(|node| resolved_accessible_name(&nodes, node));
        assert_eq!(name.as_deref(), Some("Cancel"));
    }
}
