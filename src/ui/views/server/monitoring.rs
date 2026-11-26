use arboard::Clipboard;
use eframe::egui::{self, RichText, Sense, Stroke, Ui, Vec2};

use super::chrome::{Tone, yes_no};
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
        ui.push_id("server.monitoring", |ui| {
            ui.heading(tr("Server monitoring"));
        });
        let has_monitoring = self
            .server_view
            .lifecycle_monitoring
            .as_ref()
            .is_some_and(|monitoring| monitoring.status.instance_id == id);
        if !has_monitoring {
            self.server_note(ui, tr("Monitoring data is unknown. Refresh runtime status to load the retained owner observation."));
        }
        if let Some(monitoring) = self
            .server_view
            .lifecycle_monitoring
            .as_ref()
            .filter(|monitoring| monitoring.status.instance_id == id)
        {
            self.render_monitoring_health(ui, monitoring, self.server_view.monitoring_paused);
            ui.add_space(6.0);
            self.render_monitoring_resources(ui, monitoring);
            ui.add_space(6.0);
        }
        ui.horizontal_wrapped(|ui| {
            if ui
                .add_enabled(
                    !self.server_view.controls_busy(),
                    egui::Button::new(tr("Open raw runtime log folder")),
                )
                .clicked()
            {
                self.start_server_task(ServerTaskAction::OpenInstanceRuntimeLogs(id.clone()));
            }
        });
        self.server_note(ui, tr("The runtime log folder contains unsanitized engine logs. Copy and export actions use sanitized data."));
        let busy = self.server_view.controls_busy();
        let log_bg = self.color_main_bg();
        let log_stroke = self.color_widget_bg();
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
            return;
        };
        ui.add_space(6.0);
        let (copy_result, export) = render_log_controls(
            ui,
            monitoring,
            monitoring_search,
            monitoring_paused,
            monitoring_auto_scroll,
            busy,
            (log_bg, log_stroke),
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

    fn render_monitoring_health(&self, ui: &mut Ui, monitoring: &InstanceMonitoring, paused: bool) {
        if monitoring.observation_unavailable {
            self.server_callout(ui, Tone::Warning, &tr("Cached monitoring could not be validated. Process status remains independently available."), |_| {});
        }
        let status = &monitoring.status;
        render_query_status(ui, monitoring);
        if let Some(observation) = &monitoring.observation
            && let Some(query) = &observation.query.last_good
        {
            self.server_facts(
                ui,
                "monitoring-query",
                &[
                    (tr("Server name"), query.name.clone()),
                    (tr("Map"), query.map.clone()),
                    (
                        tr("Players"),
                        tr_fmt(
                            "{players} of {max}",
                            &[
                                ("players", query.players.to_string()),
                                ("max", query.max_players.to_string()),
                            ],
                        ),
                    ),
                    (tr("Bots"), query.bots.to_string()),
                    (tr("Version"), query.version.clone()),
                ],
            );
        }
        self.server_note(ui, tr("Game FPS: Unknown; scenario readiness: Unknown."));
        if paused {
            self.server_note(ui, tr(
                "Log display paused. Status, query, and resource observations continue updating.",
            ));
        }
        egui::CollapsingHeader::new(tr("Technical details"))
            .id_salt("server-monitoring-details")
            .show(ui, |ui| {
                ui.label(tr_fmt(
                    "Current owner: {owner}; run generation: {run}; observed process: {process}",
                    &[
                        ("owner", status.owner_id.clone()),
                        ("run", status.run_generation.to_string()),
                        ("process", yes_no(status.process_exists)),
                    ],
                ));
            });
    }

    fn render_monitoring_resources(&self, ui: &mut Ui, monitoring: &InstanceMonitoring) {
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
        let state = latest
            .map(|sample| resource_label(sample.state))
            .unwrap_or_else(|| tr("Unknown"));
        let accent = self.color_primary_accent();
        let dim = self.color_text_dim();
        ui.horizontal_wrapped(|ui| {
            ui.vertical(|ui| {
                self.server_facts(
                    ui,
                    "monitoring-resources",
                    &[
                        (tr("CPU"), cpu),
                        (tr("Memory"), memory),
                        (tr("Uptime"), uptime),
                        (tr("Sample state"), state),
                    ],
                );
            });
            ui.add_space(18.0);
            sparkline(
                ui,
                &tr("CPU history"),
                points
                    .iter()
                    .filter_map(|point| point.sample.cpu_percent)
                    .map(f64::from)
                    .collect(),
                accent,
                dim,
            );
            sparkline(
                ui,
                &tr("Memory history"),
                points
                    .iter()
                    .filter_map(|point| point.sample.resident_bytes)
                    .map(|bytes| bytes as f64 / 1_048_576.0)
                    .collect(),
                accent,
                dim,
            );
        });
        self.server_note(ui, tr("CPU is measured against one logical processor and can exceed 100%. Memory is resident working-set usage."));
    }
}

pub(super) fn render_query_status(ui: &mut Ui, monitoring: &InstanceMonitoring) {
    let health = health_label(monitoring.health);
    let query = monitoring
        .observation
        .as_ref()
        .map(|observation| query_label(observation.query.state))
        .unwrap_or_else(|| tr("Unknown"));
    let message = tr_fmt(
        "Monitoring health: {health}; query state: {query}; query readiness: {ready}",
        &[
            ("health", health.clone()),
            ("query", query.clone()),
            ("ready", yes_no(monitoring.query_ready)),
        ],
    );
    let rows = [
        (tr("Monitoring health"), health),
        (tr("Server query"), query),
        (tr("Query readiness"), yes_no(monitoring.query_ready)),
        (
            tr("Observation age"),
            age_label(monitoring.observation_age_ms),
        ),
        (
            tr("Last successful query age"),
            age_label(monitoring.query_age_ms),
        ),
    ];
    let dim = ui.visuals().weak_text_color();
    let response = egui::Grid::new("server-query-status")
        .num_columns(2)
        .min_row_height(ui.text_style_height(&egui::TextStyle::Body))
        .spacing(egui::vec2(18.0, 5.0))
        .show(ui, |ui| {
            for (label, value) in rows {
                ui.label(RichText::new(label).color(dim));
                ui.label(value);
                ui.end_row();
            }
        })
        .response;
    super::workspace::server_live_status(ui, &response, &message);
}

fn sparkline(ui: &mut Ui, label: &str, values: Vec<f64>, line: egui::Color32, dim: egui::Color32) {
    ui.vertical(|ui| {
        ui.label(RichText::new(label).color(dim));
        let (response, painter) = ui.allocate_painter(Vec2::new(160.0, 36.0), Sense::hover());
        painter.rect_stroke(
            response.rect,
            egui::CornerRadius::same(4),
            Stroke::new(1.0, ui.visuals().widgets.inactive.bg_fill),
            egui::StrokeKind::Inside,
        );
        if values.len() < 2 {
            painter.text(
                response.rect.center(),
                egui::Align2::CENTER_CENTER,
                tr("Insufficient history"),
                egui::FontId::default(),
                dim,
            );
            return;
        }
        let rect = response.rect.shrink(4.0);
        let minimum = values.iter().copied().fold(f64::INFINITY, f64::min);
        let maximum = values.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        let range = (maximum - minimum).max(f64::EPSILON);
        let points = values
            .iter()
            .enumerate()
            .map(|(index, value)| {
                let x = rect.left() + rect.width() * index as f32 / (values.len() - 1) as f32;
                let y = rect.bottom() - rect.height() * ((*value - minimum) / range) as f32;
                egui::pos2(x, y)
            })
            .collect();
        painter.add(egui::Shape::line(points, Stroke::new(1.5, line)));
    });
}

fn render_log_controls(
    ui: &mut Ui,
    monitoring: &InstanceMonitoring,
    search: &mut String,
    paused: &mut bool,
    auto_scroll: &mut bool,
    busy: bool,
    (log_bg, log_stroke): (egui::Color32, egui::Color32),
) -> (
    Option<bool>,
    Option<(std::path::PathBuf, MonitoringSnapshot)>,
) {
    ui.label(RichText::new(tr("Logs")).strong());
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
    egui::Frame::new()
        .fill(log_bg)
        .stroke(Stroke::new(1.0, log_stroke))
        .corner_radius(egui::CornerRadius::same(6))
        .inner_margin(egui::Margin::same(8))
        .show(ui, |ui| {
            egui::ScrollArea::vertical()
                .id_salt("server.monitoring.logs")
                .max_height(240.0)
                .auto_shrink([false, false])
                .stick_to_bottom(*auto_scroll)
                .show(ui, |ui| {
                    for line in lines {
                        ui.label(
                            RichText::new(format!("[{}] {}", line.stream, line.text)).monospace(),
                        );
                    }
                });
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
