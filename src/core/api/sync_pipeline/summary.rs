use crate::core::utils::format::sanitize_log_url;
use log::info;
use std::time::{Duration, Instant};

/// A single stage entry in the pipeline summary table.
pub(crate) struct StageEntry {
    pub name: String,
    pub duration: Duration,
    /// Key-value metadata pairs for this stage (e.g., "files=120", "bytes=50MB").
    pub details: Vec<(&'static str, String)>,
}

impl StageEntry {
    pub fn new(name: impl Into<String>, duration: Duration) -> Self {
        Self {
            name: name.into(),
            duration,
            details: Vec::new(),
        }
    }

    pub fn with(mut self, key: &'static str, value: impl std::fmt::Display) -> Self {
        self.details.push((key, value.to_string()));
        self
    }
}

/// Collects pipeline stages and prints a formatted summary table.
pub(crate) struct PipelineSummary {
    pub operation_id: String,
    pub mode: String,
    pub repo_url: String,
    pub overall_start: Instant,
    pub stages: Vec<StageEntry>,
}

impl PipelineSummary {
    pub fn new(
        operation_id: impl Into<String>,
        mode: impl Into<String>,
        repo_url: impl Into<String>,
        start: Instant,
    ) -> Self {
        Self {
            operation_id: operation_id.into(),
            mode: mode.into(),
            repo_url: repo_url.into(),
            overall_start: start,
            stages: Vec::new(),
        }
    }

    pub fn push(&mut self, entry: StageEntry) {
        self.stages.push(entry);
    }

    /// Log a formatted ASCII table summarizing all stages.
    pub fn log_table(&self, outcome: &str) {
        let total_secs = self.overall_start.elapsed().as_secs_f64();
        let repo_label = sanitize_log_url(&self.repo_url);
        info!(
            "Pipeline summary: op={} mode={} outcome={} repo={} stages={} elapsed={:.2}s",
            self.operation_id,
            self.mode,
            outcome,
            repo_label,
            self.stages.len(),
            total_secs
        );

        // Calculate column widths based on actual content
        let name_w = self
            .stages
            .iter()
            .map(|s| s.name.len())
            .max()
            .unwrap_or(10)
            .clamp(10, 34);
        let details_w = self
            .stages
            .iter()
            .map(|s| format_details(&s.details).len())
            .max()
            .unwrap_or(20)
            .clamp(20, 64);

        let sep_w = name_w + 12 + details_w + 7; // pipes + padding

        let mut lines = Vec::with_capacity(self.stages.len() + 10);
        let sep = "=".repeat(sep_w);
        let dash = "-".repeat(sep_w);

        lines.push(sep.clone());
        lines.push(format!(" PIPELINE SUMMARY: {} [{}]", self.mode, outcome));
        lines.push(format!(" op={} repo={}", self.operation_id, repo_label));
        lines.push(dash.clone());
        lines.push(format!(
            " {:<name_w$}  {:>8}  {}",
            "Stage", "Duration", "Details"
        ));
        lines.push(dash.clone());

        for entry in &self.stages {
            let detail_str = format_details(&entry.details);
            let name_display = if entry.name.len() > name_w {
                format!("{}...", &entry.name[..name_w - 3])
            } else {
                entry.name.clone()
            };
            lines.push(format!(
                " {:<name_w$}  {:>7.2}s  {}",
                name_display,
                entry.duration.as_secs_f64(),
                detail_str,
            ));
        }

        lines.push(dash);
        lines.push(format!(
            " {:<name_w$}  {:>7.2}s  outcome={}",
            "TOTAL", total_secs, outcome,
        ));
        lines.push(sep);

        info!("{}", lines.join("\n"));
    }
}

fn format_details(details: &[(&str, String)]) -> String {
    details
        .iter()
        .map(|(k, v)| format!("{}={}", k, v))
        .collect::<Vec<_>>()
        .join(", ")
}
