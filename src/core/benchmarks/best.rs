//! "Versus best measured": the fastest saved benchmark that did the same
//! useful work under the same conditions, compared by elapsed time. This is
//! an empirical reference (convention section 2.1), distinct from any
//! modeled bound: a candidate can beat it, and a slow best proves nothing
//! about physics.

use super::record::{BenchmarkOutcome, BenchmarkRecord};

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
        Some(Self {
            kind: record.kind.slug(),
            repository_url: record.repository.url.clone(),
            repository_local_path: record.repository.local_path.clone(),
            storage_class: record.machine.repository_storage_class.clone(),
            build_kind: record.build.kind.clone(),
            useful_work,
        })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct BestComparison {
    /// Id of the fastest compatible record (possibly the candidate itself).
    pub best_id: String,
    pub best_elapsed_s: f64,
    pub candidate_elapsed_s: f64,
    /// Compatible successful records, the candidate included.
    pub samples: usize,
}

impl BestComparison {
    /// `T_best / T_actual`; above one cannot happen since the best includes
    /// the candidate, and exactly one means the candidate is the best.
    pub fn ratio(&self) -> Option<f64> {
        (self.candidate_elapsed_s > 0.0).then(|| self.best_elapsed_s / self.candidate_elapsed_s)
    }

    /// `T_actual - T_best` in seconds, zero for the best run.
    pub fn gap_s(&self) -> f64 {
        self.candidate_elapsed_s - self.best_elapsed_s
    }

    /// `100 * (T_actual / T_best - 1)`.
    pub fn slower_percent(&self) -> Option<f64> {
        (self.best_elapsed_s > 0.0)
            .then(|| 100.0 * (self.candidate_elapsed_s / self.best_elapsed_s - 1.0))
    }

    pub fn is_best(&self) -> bool {
        self.gap_s() <= 0.0
    }

    /// Only the candidate itself was compatible; there is nothing to compare.
    pub fn is_alone(&self) -> bool {
        self.samples <= 1
    }
}

/// Compare `candidate` with the fastest of `records` that shares its
/// compatibility key. Records that are not compatible or did not succeed are
/// ignored; `None` when the candidate itself has no key.
pub fn best_comparable<'a>(
    candidate: &BenchmarkRecord,
    records: impl IntoIterator<Item = &'a BenchmarkRecord>,
) -> Option<BestComparison> {
    let key = CompatibilityKey::of(candidate)?;
    let mut best_id = candidate.id.clone();
    let mut best_elapsed = candidate.elapsed_ms;
    let mut samples = 1;
    for other in records {
        if other.id == candidate.id || CompatibilityKey::of(other).as_ref() != Some(&key) {
            continue;
        }
        samples += 1;
        if other.elapsed_ms < best_elapsed {
            best_elapsed = other.elapsed_ms;
            best_id = other.id.clone();
        }
    }
    Some(BestComparison {
        best_id,
        best_elapsed_s: best_elapsed as f64 / 1000.0,
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
    fn best_is_the_fastest_compatible_record_and_reports_the_gap() {
        let candidate = record("c", BenchmarkKind::ForceRedownload, 39_700);
        let others = vec![
            record("a", BenchmarkKind::ForceRedownload, 39_400),
            record("b", BenchmarkKind::ForceRedownload, 45_100),
        ];
        let best = best_comparable(&candidate, &others).unwrap();
        assert_eq!(best.best_id, "a");
        assert_eq!(best.samples, 3);
        assert!((best.gap_s() - 0.3).abs() < 1e-9);
        assert!((best.slower_percent().unwrap() - 0.7614).abs() < 1e-3);
        assert!((best.ratio().unwrap() - 39.4 / 39.7).abs() < 1e-9);
        assert!(!best.is_best());
        assert!(!best.is_alone());
    }

    #[test]
    fn candidate_can_be_its_own_best() {
        let candidate = record("c", BenchmarkKind::ForceRedownload, 39_000);
        let others = vec![record("a", BenchmarkKind::ForceRedownload, 39_400)];
        let best = best_comparable(&candidate, &others).unwrap();
        assert_eq!(best.best_id, "c");
        assert!(best.is_best());
        assert_eq!(best.gap_s(), 0.0);
        assert_eq!(best.ratio(), Some(1.0));
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
        assert_eq!(best.samples, 2);
    }

    #[test]
    fn a_failed_candidate_has_no_comparison() {
        let mut candidate = record("c", BenchmarkKind::Recheck, 5_000);
        candidate.outcome = BenchmarkOutcome::Cancelled;
        assert!(best_comparable(&candidate, &[]).is_none());
    }
}
