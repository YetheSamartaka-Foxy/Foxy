use crate::core::utils::format::sanitize_log_url;
use crate::core::utils::speed_of_light::{SolLight, sol_line};
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
                " {:<name_w$}  {:>8.3}s  {}",
                name_display,
                entry.duration.as_secs_f64(),
                detail_str,
            ));
        }

        lines.push(dash);
        lines.push(format!(
            " {:<name_w$}  {:>8.3}s  outcome={}",
            "TOTAL", total_secs, outcome,
        ));
        lines.push(sep);

        info!("{}", lines.join("\n"));
        info!("{}", self.sol_line(outcome));

        // Every pipeline exit path lands here, success or failure, so this is the
        // one place a profiled run is guaranteed to report from.
        crate::core::utils::profiling::report(&format!(
            "op={} mode={} outcome={}",
            self.operation_id, self.mode, outcome
        ));
    }
}

impl PipelineSummary {
    /// The complete-action record (conventions/SPEED_OF_LIGHT.md): one
    /// `SOL op=sync_action` per pipeline exit, whatever the outcome, with the
    /// stage service times so the critical path can be read without the table.
    fn sol_line(&self, outcome: &str) -> String {
        let mut extras = vec![
            ("op_id", self.operation_id.clone()),
            ("mode", self.mode.clone()),
            ("outcome", outcome.to_string()),
            ("stages", self.stages.len().to_string()),
            ("timer_scope", "action_wall".to_string()),
        ];
        let names: Vec<String> = self
            .stages
            .iter()
            .map(|stage| stage_key(&stage.name))
            .collect();
        for (stage, name) in self.stages.iter().zip(&names) {
            extras.push((
                name.as_str(),
                format!("{:.3}", stage.duration.as_secs_f64()),
            ));
        }
        sol_line(
            "sync_action",
            0,
            self.overall_start.elapsed(),
            &SolLight::SelfBaseline,
            &extras,
        )
    }
}

/// `stage_<name>_s`, with the stage name reduced to the grammar's key charset.
fn stage_key(name: &str) -> String {
    let name: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect();
    format!("stage_{name}_s")
}

fn format_details(details: &[(&str, String)]) -> String {
    details
        .iter()
        .map(|(k, v)| format!("{}={}", k, v))
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stage_keys_use_the_grammar_charset() {
        assert_eq!(stage_key("remote_repository"), "stage_remote_repository_s");
        assert_eq!(stage_key("Quick Verify/2"), "stage_quick_verify_2_s");
    }

    #[test]
    fn action_record_carries_owner_outcome_and_stage_times() {
        let mut summary =
            PipelineSummary::new("sync-0007", "Download", "http://x/", Instant::now());
        summary.push(StageEntry::new(
            "remote_repository",
            Duration::from_millis(1500),
        ));
        summary.push(StageEntry::new("download", Duration::from_secs(7)));
        let line = summary.sol_line("completed");
        assert!(line.starts_with("SOL op=sync_action actual_s="));
        assert!(line.contains(
            " op_id=sync-0007 mode=Download outcome=completed stages=2 timer_scope=action_wall"
        ));
        assert!(line.contains(" stage_remote_repository_s=1.500 stage_download_s=7.000"));
    }
}
