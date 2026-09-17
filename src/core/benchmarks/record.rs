use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

pub const BENCHMARK_RECORD_VERSION: u32 = 2;

/// Which user action the benchmark measured.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, PartialOrd, Ord)]
pub enum BenchmarkKind {
    Recheck,
    QuickCheck,
    IntegrityCheck,
    Update,
    ForceRedownload,
    AddonDownload,
    AddonForceRedownload,
}

impl BenchmarkKind {
    pub const ALL: [BenchmarkKind; 7] = [
        BenchmarkKind::Recheck,
        BenchmarkKind::QuickCheck,
        BenchmarkKind::IntegrityCheck,
        BenchmarkKind::Update,
        BenchmarkKind::ForceRedownload,
        BenchmarkKind::AddonDownload,
        BenchmarkKind::AddonForceRedownload,
    ];

    /// Stable identifier used in file names, the DB index and filters.
    pub fn slug(self) -> &'static str {
        match self {
            BenchmarkKind::Recheck => "recheck",
            BenchmarkKind::QuickCheck => "quick-check",
            BenchmarkKind::IntegrityCheck => "integrity-check",
            BenchmarkKind::Update => "update",
            BenchmarkKind::ForceRedownload => "force-redownload",
            BenchmarkKind::AddonDownload => "addon-download",
            BenchmarkKind::AddonForceRedownload => "addon-force-redownload",
        }
    }

    /// English label; the UI translates it through i18n.
    pub fn label(self) -> &'static str {
        match self {
            BenchmarkKind::Recheck => "Recheck",
            BenchmarkKind::QuickCheck => "Quick check",
            BenchmarkKind::IntegrityCheck => "Integrity check",
            BenchmarkKind::Update => "Update",
            BenchmarkKind::ForceRedownload => "Force redownload",
            BenchmarkKind::AddonDownload => "Addon download",
            BenchmarkKind::AddonForceRedownload => "Addon force redownload",
        }
    }

    pub fn transfers_files(self) -> bool {
        matches!(
            self,
            BenchmarkKind::Update
                | BenchmarkKind::ForceRedownload
                | BenchmarkKind::AddonDownload
                | BenchmarkKind::AddonForceRedownload
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BenchmarkOutcome {
    Success,
    Failed { message: String },
    Cancelled,
}

impl BenchmarkOutcome {
    pub fn slug(&self) -> &'static str {
        match self {
            BenchmarkOutcome::Success => "success",
            BenchmarkOutcome::Failed { .. } => "failed",
            BenchmarkOutcome::Cancelled => "cancelled",
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct BenchmarkRepository {
    pub name: String,
    pub url: String,
    pub local_path: String,
    #[serde(default)]
    pub space_id: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct BenchmarkBuild {
    pub version: String,
    pub commit: String,
    pub kind: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct BenchmarkMachine {
    pub os: String,
    pub cpu: String,
    pub cpu_cores: usize,
    /// Logical CPUs; `cpu_percent` samples sum over them, so 100% here is
    /// `cpu_threads * 100` raw. Zero on records written before it was stored.
    #[serde(default)]
    pub cpu_threads: usize,
    pub total_memory_bytes: u64,
    pub repository_storage_class: String,
    pub hash_io_profile: String,
    pub download_speed_limit_mbps: Option<u32>,
    pub extended_diagnostics: bool,
}

/// One point of the 1 Hz series the UI records while the action runs. Rates
/// are the live values at that moment; counters are running totals.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct BenchmarkSample {
    pub t_ms: u64,
    pub downloaded_bytes: u64,
    pub download_bps: f64,
    pub disk_write_bps: f64,
    pub cpu_percent: f64,
    pub memory_bytes: u64,
    pub hash_files_done: u64,
    pub hash_files_total: u64,
    pub hash_parts_done: u64,
    pub hash_parts_total: u64,
    pub progress_percent: f32,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct BenchmarkStageMark {
    pub t_ms: u64,
    pub label: String,
}

/// A row of the `PIPELINE SUMMARY` table the core logs at the end of a run.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct BenchmarkStage {
    pub name: String,
    pub seconds: f64,
    pub details: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BenchmarkCompatibility {
    #[serde(default)]
    pub operation_label: String,
    #[serde(default)]
    pub initial_state: String,
    #[serde(default)]
    pub cache_preparation: String,
    #[serde(default)]
    pub origin_fingerprint: String,
    #[serde(default)]
    pub payload_fingerprint: String,
    #[serde(default)]
    pub algorithm: String,
    #[serde(default)]
    pub diagnostics: String,
    #[serde(default)]
    pub metric_version: u32,
    #[serde(default)]
    pub timer_scope: String,
    #[serde(default)]
    pub reference_ids: Vec<String>,
    #[serde(default = "default_baseline_frozen")]
    pub baseline_frozen: bool,
}

fn default_baseline_frozen() -> bool {
    true
}

impl Default for BenchmarkCompatibility {
    fn default() -> Self {
        Self {
            operation_label: String::new(),
            initial_state: String::new(),
            cache_preparation: String::new(),
            origin_fingerprint: String::new(),
            payload_fingerprint: String::new(),
            algorithm: String::new(),
            diagnostics: String::new(),
            metric_version: 0,
            timer_scope: String::new(),
            reference_ids: Vec::new(),
            baseline_frozen: true,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BenchmarkMetrics {
    pub downloaded_bytes: u64,
    pub planned_transfer_bytes: u64,
    pub full_download_bytes: u64,
    pub patch_savings_bytes: u64,
    pub patched_files: u64,
    pub mods_updated: u64,
    pub files_updated: u64,
    pub parts_updated: u64,
    pub download_stage_ms: u64,
    pub hash_stage_ms: u64,
    pub cumulative_hash_ms: u64,
    pub avg_download_bps: f64,
    pub peak_download_bps: f64,
    pub peak_memory_bytes: u64,
    pub avg_cpu_percent: f64,
    pub hash_files_total: u64,
    pub hash_parts_total: u64,
    /// Pending updates the check found (0 for downloads).
    pub pending_updates: u64,
    /// Conditions that must match before this record may be a measured-time
    /// reference. Empty legacy fields are derived from the record and its SOL
    /// lines when the comparison key is built.
    #[serde(default)]
    pub compatibility: BenchmarkCompatibility,
}

impl Default for BenchmarkMetrics {
    fn default() -> Self {
        Self {
            downloaded_bytes: 0,
            planned_transfer_bytes: 0,
            full_download_bytes: 0,
            patch_savings_bytes: 0,
            patched_files: 0,
            mods_updated: 0,
            files_updated: 0,
            parts_updated: 0,
            download_stage_ms: 0,
            hash_stage_ms: 0,
            cumulative_hash_ms: 0,
            avg_download_bps: 0.0,
            peak_download_bps: 0.0,
            peak_memory_bytes: 0,
            avg_cpu_percent: 0.0,
            hash_files_total: 0,
            hash_parts_total: 0,
            pending_updates: 0,
            compatibility: BenchmarkCompatibility::default(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BenchmarkRecord {
    pub id: String,
    pub record_version: u32,
    pub name: String,
    #[serde(default)]
    pub notes: String,
    pub kind: BenchmarkKind,
    /// Unix seconds when the action started.
    pub started_at: i64,
    pub started_at_local: String,
    pub finished_at_local: String,
    pub elapsed_ms: u64,
    pub outcome: BenchmarkOutcome,
    #[serde(default)]
    pub operation_id: Option<String>,
    pub repository: BenchmarkRepository,
    #[serde(default)]
    pub addons: Vec<String>,
    #[serde(default)]
    pub favourite: bool,
    #[serde(default)]
    pub hidden: bool,
    pub build: BenchmarkBuild,
    pub machine: BenchmarkMachine,
    pub metrics: BenchmarkMetrics,
    #[serde(default)]
    pub stages: Vec<BenchmarkStage>,
    /// `SOL op=...` lines from the log frame, one key/value map per line.
    #[serde(default)]
    pub sol: Vec<BTreeMap<String, String>>,
    #[serde(default)]
    pub samples: Vec<BenchmarkSample>,
    #[serde(default)]
    pub stage_marks: Vec<BenchmarkStageMark>,
    /// Name of the log slice file beside the record, when one was written.
    #[serde(default)]
    pub log_file: Option<String>,
    #[serde(default)]
    pub log_line_count: usize,
}

impl BenchmarkRecord {
    pub fn elapsed_secs(&self) -> f64 {
        self.elapsed_ms as f64 / 1000.0
    }

    /// Text the list search matches against.
    pub fn search_haystack(&self) -> String {
        let mut text = String::new();
        for part in [
            self.name.as_str(),
            self.notes.as_str(),
            self.kind.label(),
            self.repository.name.as_str(),
            self.repository.url.as_str(),
            self.outcome.slug(),
            self.started_at_local.as_str(),
        ] {
            text.push_str(part);
            text.push(' ');
        }
        for addon in &self.addons {
            text.push_str(addon);
            text.push(' ');
        }
        text.to_lowercase()
    }

    /// The rate that best summarises the action: download speed for
    /// transfers, hashed files per second for checks.
    pub fn headline_rate(&self) -> Option<(f64, &'static str)> {
        if self.kind.transfers_files() && self.metrics.avg_download_bps > 0.0 {
            return Some((self.metrics.avg_download_bps, "B/s"));
        }
        let secs = self.elapsed_secs();
        if self.metrics.hash_files_total > 0 && secs > 0.0 {
            return Some((self.metrics.hash_files_total as f64 / secs, "files/s"));
        }
        None
    }
}

/// Keep at most `max_points` samples, evenly spaced, always retaining the last.
pub fn downsample_samples(samples: &[BenchmarkSample], max_points: usize) -> Vec<BenchmarkSample> {
    if max_points == 0 || samples.len() <= max_points {
        return samples.to_vec();
    }
    let mut out = Vec::with_capacity(max_points);
    let last = samples.len() - 1;
    for index in 0..max_points {
        let source = index * last / (max_points - 1);
        out.push(samples[source]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_slugs_are_unique() {
        let mut slugs: Vec<&str> = BenchmarkKind::ALL.iter().map(|kind| kind.slug()).collect();
        slugs.dedup();
        assert_eq!(slugs.len(), BenchmarkKind::ALL.len());
    }

    #[test]
    fn downsample_keeps_endpoints_and_bound() {
        let samples: Vec<BenchmarkSample> = (0..1000)
            .map(|i| BenchmarkSample {
                t_ms: i,
                ..BenchmarkSample::default()
            })
            .collect();
        let reduced = downsample_samples(&samples, 100);
        assert_eq!(reduced.len(), 100);
        assert_eq!(reduced.first().map(|s| s.t_ms), Some(0));
        assert_eq!(reduced.last().map(|s| s.t_ms), Some(999));
        assert_eq!(downsample_samples(&samples[..10], 100).len(), 10);
        assert!(downsample_samples(&samples, 0).len() == 1000);
    }
}
