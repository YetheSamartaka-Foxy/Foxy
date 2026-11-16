use arboard::Clipboard;
use eframe::egui::{self, Sense, Stroke, Ui, Vec2};

use crate::core::server::{
    InstanceMonitoring, LogReadError, MonitoringHealth, MonitoringSnapshot, QueryState,
    ResourceState,
};
use crate::ui::app::Foxy;
use crate::ui::app::agent_support;
use crate::ui::app::server::{ServerTaskAction, ServerViewState};
use crate::ui::i18n::{tr, tr_fmt};

impl Foxy {
    pub(super) fn render_server_monitoring(&mut self, ui: &mut Ui) {
        let id = match self.server_view.selected.clone() {
            Some(id) => id,
            None => return,
        };
        ui.separator();
        ui.push_id("server.monitoring", |ui| {
            ui.heading(tr("Server monitoring"));
        });
        if ui
            .add_enabled(
                !self.server_view.controls_busy(),
                egui::Button::new(tr("Open raw runtime log folder")),
            )
            .clicked()
        {
            self.start_server_task(ServerTaskAction::OpenInstanceRuntimeLogs(id.clone()));
        }
        ui.label(tr("The runtime log folder contains unsanitized engine logs. Copy and export actions use sanitized data."));
        let busy = self.server_view.controls_busy();
        let ServerViewState {
            lifecycle_monitoring,
            monitoring_paused,
            monitoring_auto_scroll,
            monitoring_search,
            ..
        } = &mut self.server_view;
        let Some(monitoring) = lifecycle_monitoring
            .as_ref()
            .filter(|monitoring| monitoring.status.instance_id == id)
        else {
            ui.label(tr("Monitoring data is unknown. Refresh runtime status to load the retained owner observation."));
            return;
        };
        render_health(ui, monitoring, *monitoring_paused);
        render_resources(ui, monitoring);
        let (copy_result, export) = render_log_controls(
            ui,
            monitoring,
            monitoring_search,
            monitoring_paused,
            monitoring_auto_scroll,
            busy,
        );
        if let Some(copied) = copy_result {
            if copied {
                self.show_success_toast(self.t("Visible server logs copied."));
            } else {
                self.show_error_toast(self.t("Unable to copy server logs."));
            }
        }
        if let Some((path, snapshot)) = export {
            self.start_server_task(ServerTaskAction::ExportMonitoringSnapshot {
                snapshot: Box::new(snapshot),
                path,
            });
        }
    }
}

fn render_health(ui: &mut Ui, monitoring: &InstanceMonitoring, paused: bool) {
    if monitoring.observation_unavailable {
        ui.label(tr("Cached monitoring could not be validated. Process status remains independently available."));
    }
    let status = &monitoring.status;
    ui.label(tr_fmt(
        "Monitoring health: {health}; query state: {query}; query readiness: {ready}",
        &[
            ("health", health_label(monitoring.health)),
            (
                "query",
                monitoring
                    .observation
                    .as_ref()
                    .map(|observation| query_label(observation.query.state))
                    .unwrap_or_else(|| tr("Unknown")),
            ),
            (
                "ready",
                if monitoring.query_ready {
                    tr("Yes")
                } else {
                    tr("No")
                },
            ),
        ],
    ));
    ui.label(tr_fmt(
        "Current owner: {owner}; run generation: {run}; observed process: {process}",
        &[
            ("owner", status.owner_id.clone()),
            ("run", status.run_generation.to_string()),
            ("process", status.process_exists.to_string()),
        ],
    ));
    ui.label(tr_fmt(
        "Observation age: {observation}; last successful query age: {query}",
        &[
            ("observation", age_label(monitoring.observation_age_ms)),
            ("query", age_label(monitoring.query_age_ms)),
        ],
    ));
    if paused {
        ui.label(tr(
            "Log display paused. Status, query, and resource observations continue updating.",
        ));
    }
    if let Some(observation) = &monitoring.observation
        && let Some(query) = &observation.query.last_good
    {
        ui.label(tr_fmt(
                "Server: {name}; map: {map}; players: {players} of {max}; bots: {bots}; version: {version}",
                &[
                    ("name", query.name.clone()),
                    ("map", query.map.clone()),
                    ("players", query.players.to_string()),
                    ("max", query.max_players.to_string()),
                    ("bots", query.bots.to_string()),
                    ("version", query.version.clone()),
                ],
            ));
    }
    ui.label(tr("Game FPS: Unknown; scenario readiness: Unknown."));
}

fn render_resources(ui: &mut Ui, monitoring: &InstanceMonitoring) {
    ui.label(tr("CPU is measured against one logical processor and can exceed 100%. Memory is resident working-set usage."));
    let points = monitoring
        .observation
        .as_ref()
        .map(|observation| observation.resources.as_slice())
        .unwrap_or_default();
    let latest = points.last().map(|point| &point.sample);
    let cpu = latest
        .and_then(|sample| sample.cpu_percent)
        .map(|value| format!("{value:.1}%"))
        .unwrap_or_else(|| tr("Unknown"));
    let memory = latest
        .and_then(|sample| sample.resident_bytes)
        .map(|bytes| format!("{:.1} MiB", bytes as f64 / 1_048_576.0))
        .unwrap_or_else(|| tr("Unknown"));
    let uptime = latest
        .and_then(|sample| sample.uptime_ms)
        .map(format_duration)
        .unwrap_or_else(|| tr("Unknown"));
    ui.label(tr_fmt(
        "Runtime resources: CPU {cpu}; memory {memory}; uptime {uptime}; sample state {state}",
        &[
            ("cpu", cpu),
            ("memory", memory),
            ("uptime", uptime),
            (
                "state",
                latest
                    .map(|sample| resource_label(sample.state))
                    .unwrap_or_else(|| tr("Unknown")),
            ),
        ],
    ));
    ui.horizontal(|ui| {
        sparkline(
            ui,
            &tr("CPU history"),
            points
                .iter()
                .filter_map(|point| point.sample.cpu_percent)
                .map(f64::from)
                .collect(),
        );
        sparkline(
            ui,
            &tr("Memory history"),
            points
                .iter()
                .filter_map(|point| point.sample.resident_bytes)
                .map(|bytes| bytes as f64 / 1_048_576.0)
                .collect(),
        );
    });
}

fn sparkline(ui: &mut Ui, label: &str, values: Vec<f64>) {
    ui.vertical(|ui| {
        ui.label(label);
        let (response, painter) = ui.allocate_painter(Vec2::new(150.0, 30.0), Sense::hover());
        if values.len() < 2 {
            painter.text(
                response.rect.center(),
                egui::Align2::CENTER_CENTER,
                tr("Insufficient history"),
                egui::FontId::default(),
                ui.visuals().text_color(),
            );
            return;
        }
        let minimum = values.iter().copied().fold(f64::INFINITY, f64::min);
        let maximum = values.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        let range = (maximum - minimum).max(f64::EPSILON);
        let points = values
            .iter()
            .enumerate()
            .map(|(index, value)| {
                let x = response.rect.left()
                    + response.rect.width() * index as f32 / (values.len() - 1) as f32;
                let y = response.rect.bottom()
                    - response.rect.height() * ((*value - minimum) / range) as f32;
                egui::pos2(x, y)
            })
            .collect();
        painter.add(egui::Shape::line(
            points,
            Stroke::new(1.5, ui.visuals().selection.bg_fill),
        ));
    });
}

fn render_log_controls(
    ui: &mut Ui,
    monitoring: &InstanceMonitoring,
    search: &mut String,
    paused: &mut bool,
    auto_scroll: &mut bool,
    busy: bool,
) -> (
    Option<bool>,
    Option<(std::path::PathBuf, MonitoringSnapshot)>,
) {
    ui.horizontal_wrapped(|ui| {
        ui.checkbox(paused, tr("Pause log display"));
        ui.checkbox(auto_scroll, tr("Auto-scroll logs"));
        let search_label = ui.label(tr("Search log text"));
        ui.add(egui::TextEdit::singleline(search).desired_width(180.0))
            .labelled_by(search_label.id);
    });
    let Some(snapshot) = monitoring
        .observation
        .as_ref()
        .map(|observation| &observation.logs)
    else {
        ui.label(tr("No log snapshot is available."));
        return (None, None);
    };
    let needle = search.to_lowercase();
    let lines = snapshot
        .entries
        .iter()
        .filter(|line| needle.is_empty() || line.text.to_lowercase().contains(&needle))
        .collect::<Vec<_>>();
    ui.label(tr_fmt(
        "Showing {shown} of {total} retained log lines. Severity is unclassified raw text.",
        &[
            ("shown", lines.len().to_string()),
            ("total", snapshot.entries.len().to_string()),
        ],
    ));
    if snapshot.truncated {
        ui.label(tr(
            "The bounded log snapshot omitted older or oversized content.",
        ));
    }
    if let Some(error) = snapshot.error {
        ui.label(tr(log_error_label(error)));
    }
    let mut copy_result = None;
    let mut export = None;
    ui.horizontal_wrapped(|ui| {
        if ui
            .add_enabled(
                !lines.is_empty(),
                egui::Button::new(tr("Copy visible logs")),
            )
            .clicked()
        {
            let text = lines
                .iter()
                .map(|line| format!("[{}] {}", line.stream, line.text))
                .collect::<Vec<_>>()
                .join("\n");
            copy_result = Some(
                Clipboard::new()
                    .and_then(|mut clipboard| clipboard.set_text(text))
                    .is_ok(),
            );
        }
        if ui
            .add_enabled(!busy, egui::Button::new(tr("Export sanitized snapshot")))
            .clicked()
            && let Some(path) = agent_support::save_file(|| {
                rfd::FileDialog::new()
                    .add_filter(tr("JSON snapshot"), &["json"])
                    .set_file_name("server-monitoring.json")
                    .save_file()
            })
        {
            export = monitoring
                .observation
                .as_ref()
                .map(|observation| (path, observation.clone()));
        }
    });
    egui::ScrollArea::vertical()
        .id_salt("server.monitoring.logs")
        .max_height(240.0)
        .auto_shrink([false, false])
        .stick_to_bottom(*auto_scroll)
        .show(ui, |ui| {
            for line in lines {
                ui.label(format!("[{}] {}", line.stream, line.text));
            }
        });
    (copy_result, export)
}

fn health_label(health: MonitoringHealth) -> String {
    tr(match health {
        MonitoringHealth::NotRunning => "Not running",
        MonitoringHealth::Unknown => "Unknown",
        MonitoringHealth::Starting => "Starting",
        MonitoringHealth::Healthy => "Healthy",
        MonitoringHealth::Degraded => "Degraded",
        MonitoringHealth::Stale => "Stale",
    })
}

fn query_label(state: QueryState) -> String {
    tr(match state {
        QueryState::Unavailable => "Unavailable",
        QueryState::Pending => "Pending",
        QueryState::Responding => "Responding",
        QueryState::Timeout => "Timed out",
        QueryState::InvalidResponse => "Invalid response",
        QueryState::NetworkError => "Network error",
        QueryState::IdentityMismatch => "Identity mismatch",
        QueryState::Cancelled => "Cancelled",
    })
}

fn resource_label(state: ResourceState) -> String {
    tr(match state {
        ResourceState::Available => "Available",
        ResourceState::WarmingUp => "Warming up",
        ResourceState::Unavailable => "Unavailable",
        ResourceState::IdentityChanged => "Process identity changed",
    })
}

fn log_error_label(error: LogReadError) -> &'static str {
    match error {
        LogReadError::DiscoveryLimit => "Log discovery reached its safety limit.",
        LogReadError::ReadFailed => "Some log files could not be read.",
        LogReadError::UnsafePath => "An unsafe log path was skipped.",
    }
}

fn age_label(milliseconds: Option<u64>) -> String {
    milliseconds
        .map(format_duration)
        .unwrap_or_else(|| tr("Unknown"))
}

fn format_duration(milliseconds: u64) -> String {
    let seconds = milliseconds / 1000;
    tr_fmt(
        "{minutes} min {seconds} sec",
        &[
            ("minutes", (seconds / 60).to_string()),
            ("seconds", (seconds % 60).to_string()),
        ],
    )
}
