//! "Versus best measured": the median of a frozen set of saved benchmarks
//! that did the same useful work under the same conditions. This is
//! an empirical reference (convention section 2.1), distinct from any
//! modeled bound: a candidate can beat it, and a slow best proves nothing
//! about physics.

use std::collections::BTreeSet;

use super::record::{BenchmarkCompatibility, BenchmarkOutcome, BenchmarkRecord};

/// The conditions two records must share before their elapsed times can be
/// compared: the same action on the same repository instance, on the same
/// storage class and build kind, and the same amount of required work.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompatibilityKey {
    pub kind: &'static str,
    pub repository_url: String,
    pub repository_local_path: String,
    pub storage_class: String,
    pub build_kind: String,
    /// `(files, bytes)` for transfers, `(files, parts)` for checks.
    pub useful_work: (u64, u64),
    pub operation_label: String,
    pub initial_state: String,
    pub cache_preparation: String,
    pub origin_fingerprint: String,
    pub payload_fingerprint: String,
    pub algorithm: String,
    pub diagnostics: String,
    pub metric_version: u32,
    pub timer_scope: String,
    pub reference_ids: Vec<String>,
}

impl CompatibilityKey {
    /// `None` for a record that cannot serve as a reference: a failed or
    /// cancelled run, or one whose useful work is unknown.
    pub fn of(record: &BenchmarkRecord) -> Option<Self> {
        if record.outcome != BenchmarkOutcome::Success {
            return None;
        }
        let m = &record.metrics;
        let useful_work = if record.kind.transfers_files() {
            (m.files_updated, m.full_download_bytes)
        } else {
            (m.hash_files_total, m.hash_parts_total)
        };
        let compatibility = compatibility_of(record);
        Some(Self {
            kind: record.kind.slug(),
            repository_url: record.repository.url.clone(),
            repository_local_path: record.repository.local_path.clone(),
            storage_class: record.machine.repository_storage_class.clone(),
            build_kind: record.build.kind.clone(),
            useful_work,
            operation_label: compatibility.operation_label,
            initial_state: compatibility.initial_state,
            cache_preparation: compatibility.cache_preparation,
            origin_fingerprint: compatibility.origin_fingerprint,
            payload_fingerprint: compatibility.payload_fingerprint,
            algorithm: compatibility.algorithm,
            diagnostics: compatibility.diagnostics,
            metric_version: compatibility.metric_version,
            timer_scope: compatibility.timer_scope,
            reference_ids: compatibility.reference_ids,
        })
    }
}

fn compatibility_of(record: &BenchmarkRecord) -> BenchmarkCompatibility {
    let saved = &record.metrics.compatibility;
    let values = |key: &str| {
        let mut values: BTreeSet<String> = record
            .sol
            .iter()
            .filter_map(|line| line.get(key))
            .filter(|value| !value.is_empty() && value.as_str() != "na")
            .cloned()
            .collect();
        values.pop_first().map_or_else(String::new, |first| {
            std::iter::once(first)
                .chain(values)
                .collect::<Vec<_>>()
                .join(",")
        })
    };
    let fallback = |saved: &str, derived: String| {
        if saved.is_empty() {
            derived
        } else {
            saved.to_owned()
        }
    };
    let operation_label = record
        .sol
        .iter()
        .rev()
        .find(|line| matches!(line.get("op").map(String::as_str), Some("sync_action")))
        .map(|line| {
            line.get("mode").map_or_else(
                || "sync_action".to_owned(),
                |mode| format!("sync_action:{mode}"),
            )
        })
        .unwrap_or_else(|| record.kind.slug().to_owned());
    let initial_state = values("initial_state");
    let cache_preparation = {
        let value = values("cache_preparation");
        if value.is_empty() {
            values("cache_state")
        } else {
            value
        }
    };
    let metric_version = record
        .sol
        .iter()
        .filter_map(|line| line.get("metric_version"))
        .filter_map(|value| value.parse::<u32>().ok())
        .max()
        .unwrap_or(1);
    let reference_ids = {
        let mut ids: BTreeSet<String> = record
            .sol
            .iter()
            .filter_map(|line| line.get("reference_id"))
            .filter(|value| !value.is_empty() && value.as_str() != "na")
            .cloned()
            .collect();
        if ids.is_empty() {
            ids.extend(
                record
                    .sol
                    .iter()
                    .filter_map(|line| line.get("light_src"))
                    .filter(|value| !value.is_empty())
                    .map(|value| format!("light_src:{value}")),
            );
        }
        ids.into_iter().collect::<Vec<_>>()
    };
    let payload_fingerprint = if record.kind.transfers_files() {
        format!(
            "files={};full_bytes={};patch_bytes={}",
            record.metrics.files_updated,
            record.metrics.full_download_bytes,
            record.metrics.patch_savings_bytes
        )
    } else {
        format!(
            "hash_files={};hash_parts={}",
            record.metrics.hash_files_total, record.metrics.hash_parts_total
        )
    };
    BenchmarkCompatibility {
        operation_label: fallback(&saved.operation_label, operation_label),
        initial_state: fallback(
            &saved.initial_state,
            if initial_state.is_empty() {
                record.kind.slug().to_owned()
            } else {
                initial_state
            },
        ),
        cache_preparation: fallback(
            &saved.cache_preparation,
            if cache_preparation.is_empty() {
                "not-recorded".to_owned()
            } else {
                cache_preparation
            },
        ),
        origin_fingerprint: fallback(&saved.origin_fingerprint, record.repository.url.clone()),
        payload_fingerprint: fallback(&saved.payload_fingerprint, payload_fingerprint),
        algorithm: fallback(&saved.algorithm, {
            let algorithm = values("algorithm");
            if algorithm.is_empty() {
                "not-recorded".to_owned()
            } else {
                algorithm
            }
        }),
        diagnostics: fallback(
            &saved.diagnostics,
            record.machine.extended_diagnostics.to_string(),
        ),
        metric_version: if saved.metric_version == 0 {
            metric_version
        } else {
            saved.metric_version
        },
        timer_scope: fallback(&saved.timer_scope, {
            let scope = values("timer_scope");
            if scope.is_empty() {
                "not-recorded".to_owned()
            } else {
                scope
            }
        }),
        reference_ids: if saved.reference_ids.is_empty() {
            reference_ids
        } else {
            let mut ids = saved.reference_ids.clone();
            ids.sort();
            ids.dedup();
            ids
        },
        baseline_frozen: saved.baseline_frozen,
    }
}

pub fn populate_compatibility(record: &mut BenchmarkRecord) {
    record.metrics.compatibility = compatibility_of(record);
}

#[derive(Clone, Debug, PartialEq)]
pub struct BestComparison {
    /// Id of the prior record nearest the frozen-set median.
    pub best_id: String,
    /// Median elapsed time of the compatible frozen prior records.
    pub best_elapsed_s: f64,
    /// Fastest compatible prior record, retained as secondary evidence.
    pub fastest_id: String,
    pub fastest_elapsed_s: f64,
    pub candidate_elapsed_s: f64,
    /// Compatible frozen successful prior records; the candidate is excluded.
    pub samples: usize,
}

impl BestComparison {
    /// `T_frozen_median / T_actual`; above one means the candidate improved
    /// on the frozen reference set.
    pub fn ratio(&self) -> Option<f64> {
        (self.candidate_elapsed_s > 0.0).then(|| self.best_elapsed_s / self.candidate_elapsed_s)
    }

    /// `T_actual - T_frozen_median` in seconds.
    pub fn gap_s(&self) -> f64 {
        self.candidate_elapsed_s - self.best_elapsed_s
    }

    /// `100 * (T_actual / T_frozen_median - 1)`.
    pub fn slower_percent(&self) -> Option<f64> {
        (self.best_elapsed_s > 0.0)
            .then(|| 100.0 * (self.candidate_elapsed_s / self.best_elapsed_s - 1.0))
    }

    pub fn is_best(&self) -> bool {
        self.gap_s() <= 0.0
    }

    /// No frozen prior record was compatible; there is nothing to compare.
    pub fn is_alone(&self) -> bool {
        self.samples == 0
    }
}

/// Compare `candidate` with the median of the compatible frozen records that
/// existed when it started. The candidate and newer records are excluded.
pub fn best_comparable<'a>(
    candidate: &BenchmarkRecord,
    records: impl IntoIterator<Item = &'a BenchmarkRecord>,
) -> Option<BestComparison> {
    let key = CompatibilityKey::of(candidate)?;
    let mut compatible = Vec::new();
    for other in records {
        if other.id == candidate.id
            || other.started_at > candidate.started_at
            || !other.metrics.compatibility.baseline_frozen
            || CompatibilityKey::of(other).as_ref() != Some(&key)
        {
            continue;
        }
        compatible.push(other);
    }
    compatible.sort_by(|a, b| a.elapsed_ms.cmp(&b.elapsed_ms).then(a.id.cmp(&b.id)));
    let samples = compatible.len();
    let Some(fastest) = compatible.first() else {
        return Some(BestComparison {
            best_id: candidate.id.clone(),
            best_elapsed_s: candidate.elapsed_secs(),
            fastest_id: candidate.id.clone(),
            fastest_elapsed_s: candidate.elapsed_secs(),
            candidate_elapsed_s: candidate.elapsed_secs(),
            samples: 0,
        });
    };
    let middle = samples / 2;
    let median_ms = if samples % 2 == 0 {
        (compatible[middle - 1].elapsed_ms as f64 + compatible[middle].elapsed_ms as f64) / 2.0
    } else {
        compatible[middle].elapsed_ms as f64
    };
    let median_record = compatible[middle];
    Some(BestComparison {
        best_id: median_record.id.clone(),
        best_elapsed_s: median_ms / 1000.0,
        fastest_id: fastest.id.clone(),
        fastest_elapsed_s: fastest.elapsed_ms as f64 / 1000.0,
        candidate_elapsed_s: candidate.elapsed_secs(),
        samples,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::benchmarks::record::*;

    fn record(id: &str, kind: BenchmarkKind, elapsed_ms: u64) -> BenchmarkRecord {
        BenchmarkRecord {
            id: id.into(),
            record_version: 1,
            name: id.into(),
            notes: String::new(),
            kind,
            started_at: 0,
            started_at_local: String::new(),
            finished_at_local: String::new(),
            elapsed_ms,
            outcome: BenchmarkOutcome::Success,
            operation_id: None,
            repository: BenchmarkRepository {
                name: "r".into(),
                url: "https://example.test/repo/".into(),
                local_path: "D:/mods".into(),
                space_id: None,
            },
            addons: vec![],
            favourite: false,
            hidden: false,
            build: BenchmarkBuild {
                kind: "release".into(),
                ..BenchmarkBuild::default()
            },
            machine: BenchmarkMachine {
                repository_storage_class: "ssd".into(),
                ..BenchmarkMachine::default()
            },
            metrics: BenchmarkMetrics {
                files_updated: 217,
                full_download_bytes: 4_331_121_846,
                hash_files_total: 217,
                hash_parts_total: 3744,
                ..BenchmarkMetrics::default()
            },
            stages: vec![],
            sol: vec![],
            samples: vec![],
            stage_marks: vec![],
            log_file: None,
            log_line_count: 0,
        }
    }

    #[test]
    fn frozen_median_is_primary_and_fastest_is_secondary() {
        let candidate = record("c", BenchmarkKind::ForceRedownload, 39_700);
        let others = vec![
            record("a", BenchmarkKind::ForceRedownload, 39_400),
            record("b", BenchmarkKind::ForceRedownload, 45_100),
        ];
        let best = best_comparable(&candidate, &others).unwrap();
        assert_eq!(best.best_id, "b");
        assert_eq!(best.samples, 2);
        assert_eq!(best.fastest_id, "a");
        assert_eq!(best.fastest_elapsed_s, 39.4);
        assert!((best.best_elapsed_s - 42.25).abs() < 1e-9);
        assert!((best.gap_s() + 2.55).abs() < 1e-9);
        assert!((best.ratio().unwrap() - 42.25 / 39.7).abs() < 1e-9);
        assert!(best.is_best());
        assert!(!best.is_alone());
    }

    #[test]
    fn candidate_can_exceed_one_against_the_frozen_reference() {
        let candidate = record("c", BenchmarkKind::ForceRedownload, 39_000);
        let others = vec![record("a", BenchmarkKind::ForceRedownload, 39_400)];
        let best = best_comparable(&candidate, &others).unwrap();
        assert_eq!(best.best_id, "a");
        assert!(best.is_best());
        assert!((best.gap_s() + 0.4).abs() < 1e-9);
        assert_eq!(best.ratio(), Some(39.4 / 39.0));
    }

    #[test]
    fn incompatible_records_are_ignored() {
        let candidate = record("c", BenchmarkKind::ForceRedownload, 39_700);
        let mut other_kind = record("k", BenchmarkKind::Update, 1_000);
        other_kind.metrics.files_updated = 217;
        let mut other_folder = record("f", BenchmarkKind::ForceRedownload, 1_000);
        other_folder.repository.local_path = "E:/other".into();
        let mut other_work = record("w", BenchmarkKind::ForceRedownload, 1_000);
        other_work.metrics.files_updated = 40;
        let mut other_storage = record("s", BenchmarkKind::ForceRedownload, 1_000);
        other_storage.machine.repository_storage_class = "hdd".into();
        let mut other_build = record("d", BenchmarkKind::ForceRedownload, 1_000);
        other_build.build.kind = "debug".into();
        let mut failed = record("x", BenchmarkKind::ForceRedownload, 1_000);
        failed.outcome = BenchmarkOutcome::Failed {
            message: "boom".into(),
        };
        let others = vec![
            other_kind,
            other_folder,
            other_work,
            other_storage,
            other_build,
            failed,
        ];
        let best = best_comparable(&candidate, &others).unwrap();
        assert_eq!(best.best_id, "c");
        assert!(best.is_alone());
    }

    #[test]
    fn checks_compare_by_hashed_scope() {
        let candidate = record("c", BenchmarkKind::Recheck, 5_000);
        let mut same = record("a", BenchmarkKind::Recheck, 4_000);
        same.metrics.full_download_bytes = 0;
        let mut narrower = record("n", BenchmarkKind::Recheck, 1_000);
        narrower.metrics.hash_files_total = 10;
        let best = best_comparable(&candidate, &[same, narrower]).unwrap();
        assert_eq!(best.best_id, "a");
        assert_eq!(best.samples, 1);
    }

    #[test]
    fn compatibility_snapshot_rejects_every_semantic_dimension() {
        let candidate = record("c", BenchmarkKind::ForceRedownload, 5_000);
        let dimensions: Vec<fn(&mut BenchmarkCompatibility)> = vec![
            |key| key.operation_label = "other".into(),
            |key| key.initial_state = "other".into(),
            |key| key.cache_preparation = "other".into(),
            |key| key.origin_fingerprint = "other".into(),
            |key| key.payload_fingerprint = "other".into(),
            |key| key.algorithm = "other".into(),
            |key| key.diagnostics = "other".into(),
            |key| key.metric_version = 99,
            |key| key.timer_scope = "other".into(),
            |key| key.reference_ids = vec!["other".into()],
        ];
        for change in dimensions {
            let mut other = candidate.clone();
            other.id = "other".into();
            change(&mut other.metrics.compatibility);
            assert!(best_comparable(&candidate, &[other]).unwrap().is_alone());
        }
    }

    #[test]
    fn newer_and_non_frozen_records_are_not_references() {
        let mut candidate = record("c", BenchmarkKind::ForceRedownload, 5_000);
        candidate.started_at = 10;
        let mut newer = record("new", BenchmarkKind::ForceRedownload, 1_000);
        newer.started_at = 11;
        let mut provisional = record("provisional", BenchmarkKind::ForceRedownload, 1_000);
        provisional.metrics.compatibility.baseline_frozen = false;
        let best = best_comparable(&candidate, &[newer, provisional]).unwrap();
        assert!(best.is_alone());
    }

    #[test]
    fn addon_force_redownload_uses_transfer_work() {
        let candidate = record("c", BenchmarkKind::AddonForceRedownload, 5_000);
        let mut other = record("a", BenchmarkKind::AddonForceRedownload, 4_000);
        other.metrics.hash_files_total = 0;
        other.metrics.hash_parts_total = 0;
        assert_eq!(best_comparable(&candidate, &[other]).unwrap().samples, 1);
    }

    #[test]
    fn a_failed_candidate_has_no_comparison() {
        let mut candidate = record("c", BenchmarkKind::Recheck, 5_000);
        candidate.outcome = BenchmarkOutcome::Cancelled;
        assert!(best_comparable(&candidate, &[]).is_none());
    }
}
