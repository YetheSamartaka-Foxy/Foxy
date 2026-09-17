//! Package one benchmark for sharing: the record, its log slice, a flat
//! metrics text and any chart images the UI rendered.

use std::io::Write;
use std::path::Path;

use anyhow::{Context, Result};
use zip::CompressionMethod;
use zip::write::SimpleFileOptions;

use super::best::BestComparison;
use super::record::BenchmarkRecord;
use crate::core::utils::format::redact_log_text;

pub struct ExportImage {
    pub file_name: String,
    pub png: Vec<u8>,
}

pub fn write_zip(
    dest: &Path,
    record: &BenchmarkRecord,
    best: Option<&BestComparison>,
    log_text: Option<&str>,
    images: &[ExportImage],
) -> Result<()> {
    let file = std::fs::File::create(dest).with_context(|| format!("create {}", dest.display()))?;
    let mut zip = zip::ZipWriter::new(file);
    let deflate = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    let stored = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);

    zip.start_file("benchmark.json", deflate)?;
    zip.write_all(&serde_json::to_vec_pretty(record)?)?;

    zip.start_file("summary.txt", deflate)?;
    zip.write_all(summary_text(record, best).as_bytes())?;

    if let Some(text) = log_text {
        zip.start_file("benchmark.log", deflate)?;
        for line in text.lines() {
            zip.write_all(redact_log_text(line).as_bytes())?;
            zip.write_all(b"\n")?;
        }
    }

    for image in images {
        zip.start_file(format!("charts/{}", image.file_name), stored)?;
        zip.write_all(&image.png)?;
    }

    zip.finish().context("finalize benchmark zip")?;
    Ok(())
}

/// Human-readable digest of the record for people who do not open the JSON.
/// Operation and ratio come first, then the best-measured comparison, then
/// the run metadata and counters.
pub fn summary_text(record: &BenchmarkRecord, best: Option<&BestComparison>) -> String {
    let mut out = String::new();
    let mut line = |key: &str, value: String| {
        out.push_str(key);
        out.push_str(": ");
        out.push_str(&value);
        out.push('\n');
    };
    line("operation", record.headline_op().to_owned());
    let headline = record.headline_summary();
    match record.headline_sol().and_then(|summary| summary.sol) {
        Some(sol) => {
            line("sol", format!("{:.3}", sol));
            line(
                "sol_kind",
                headline
                    .as_ref()
                    .map_or("none", |summary| summary.metric_kind().label())
                    .to_owned(),
            );
        }
        None => {
            line("sol", "na".to_owned());
            line(
                "sol_kind",
                headline
                    .as_ref()
                    .map_or("reference missing", |summary| {
                        if summary.sol.is_none() {
                            if summary.heterogeneous {
                                "batches differ"
                            } else {
                                "reference missing"
                            }
                        } else if !summary.completed() {
                            "not completed"
                        } else if summary.mixed_references {
                            "mixed references"
                        } else {
                            "partial coverage"
                        }
                    })
                    .to_owned(),
            );
        }
    }
    if let Some(summary) = headline.as_ref() {
        if let Some(raw) = summary.sol_raw {
            line("sol_raw", format!("{raw:.4}"));
        }
        line("sol_light_source", summary.light_src.label().to_owned());
        line(
            "sol_coverage",
            format!("{}/{}", summary.rated_runs, summary.runs),
        );
    }
    match best {
        Some(best) if !best.is_alone() => {
            line("best_measured_id", best.best_id.clone());
            line(
                "best_measured_elapsed_s",
                format!("{:.2}", best.best_elapsed_s),
            );
            line("gap_to_best_s", format!("{:+.2}", best.gap_s()));
            if let Some(ratio) = best.ratio() {
                line("best_measured_ratio", format!("{ratio:.3}"));
            }
            line("compatible_runs", best.samples.to_string());
        }
        Some(_) => line("best_measured", "no comparable run".to_owned()),
        None => line("best_measured", "not comparable".to_owned()),
    }
    line("name", record.name.clone());
    line("kind", record.kind.label().to_owned());
    line("outcome", record.outcome.slug().to_owned());
    line("started", record.started_at_local.clone());
    line("finished", record.finished_at_local.clone());
    line("elapsed_s", format!("{:.2}", record.elapsed_secs()));
    line("repository", record.repository.name.clone());
    line("repository_url", record.repository.url.clone());
    if !record.addons.is_empty() {
        line("addons", record.addons.join(", "));
    }
    line(
        "build",
        format!(
            "{} {} {}",
            record.build.version, record.build.commit, record.build.kind
        ),
    );
    line(
        "machine",
        format!(
            "{} | {} x{} | {:.1} GB RAM | repo storage {} | hash profile {}",
            record.machine.os,
            record.machine.cpu,
            record.machine.cpu_cores,
            record.machine.total_memory_bytes as f64 / 1024.0 / 1024.0 / 1024.0,
            record.machine.repository_storage_class,
            record.machine.hash_io_profile
        ),
    );
    let m = &record.metrics;
    line("downloaded_bytes", m.downloaded_bytes.to_string());
    line(
        "planned_transfer_bytes",
        m.planned_transfer_bytes.to_string(),
    );
    line("full_download_bytes", m.full_download_bytes.to_string());
    line("patch_savings_bytes", m.patch_savings_bytes.to_string());
    line("patched_files", m.patched_files.to_string());
    line("mods_updated", m.mods_updated.to_string());
    line("files_updated", m.files_updated.to_string());
    line("parts_updated", m.parts_updated.to_string());
    line(
        "download_stage_s",
        format!("{:.2}", m.download_stage_ms as f64 / 1000.0),
    );
    line(
        "hash_stage_s",
        format!("{:.2}", m.hash_stage_ms as f64 / 1000.0),
    );
    line("avg_download_bps", format!("{:.0}", m.avg_download_bps));
    line("peak_download_bps", format!("{:.0}", m.peak_download_bps));
    line("peak_memory_bytes", m.peak_memory_bytes.to_string());
    line("avg_cpu_percent", format!("{:.1}", m.avg_cpu_percent));
    line("hash_files_total", m.hash_files_total.to_string());
    line("hash_parts_total", m.hash_parts_total.to_string());
    line("pending_updates", m.pending_updates.to_string());
    if !record.stages.is_empty() {
        out.push_str("\nstages:\n");
        for stage in &record.stages {
            out.push_str(&format!(
                "  {:<28} {:>9.3}s  {}\n",
                stage.name, stage.seconds, stage.details
            ));
        }
    }
    let summaries = record.sol_summaries();
    if !summaries.is_empty() {
        out.push_str("\noperations:\n");
        for summary in &summaries {
            let sol = summary
                .sol
                .map_or_else(|| "na".to_owned(), |sol| format!("{sol:.3}"));
            let mut fields = vec![
                format!("op={}", summary.op),
                format!("sol={sol}"),
                format!("kind={}", summary.metric_kind().label()),
                format!("runs={}", summary.runs),
                format!("rated_runs={}", summary.rated_runs),
                format!("service_s={:.3}", summary.actual_s),
            ];
            if summary.work_bytes > 0 {
                fields.push(format!("work_bytes={}", summary.work_bytes));
            }
            if let Some(raw) = summary.sol_raw {
                fields.push(format!("sol_raw={raw:.4}"));
            }
            if let Some(gap) = summary.gap_to_reference_s() {
                fields.push(format!("gap_to_reference_s={gap:+.3}"));
            }
            if !summary.outcomes.is_empty() {
                fields.push(format!("outcome={}", summary.outcomes.join(",")));
            }
            out.push_str("  ");
            out.push_str(&fields.join(" "));
            out.push('\n');
        }
    }
    if !record.sol.is_empty() {
        out.push_str("\nspeed of light:\n");
        for sol in &record.sol {
            let pairs: Vec<String> = sol.iter().map(|(k, v)| format!("{k}={v}")).collect();
            out.push_str("  ");
            out.push_str(&pairs.join(" "));
            out.push('\n');
        }
    }
    if !record.notes.trim().is_empty() {
        out.push_str("\nnotes:\n");
        out.push_str(record.notes.trim());
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::benchmarks::record::*;
    use std::io::Read;

    fn record() -> BenchmarkRecord {
        BenchmarkRecord {
            id: "bm-1".into(),
            record_version: BENCHMARK_RECORD_VERSION,
            name: "Test".into(),
            notes: "note".into(),
            kind: BenchmarkKind::Update,
            started_at: 0,
            started_at_local: "2026".into(),
            finished_at_local: "2026".into(),
            elapsed_ms: 1500,
            outcome: BenchmarkOutcome::Success,
            operation_id: None,
            repository: BenchmarkRepository::default(),
            addons: vec![],
            favourite: false,
            hidden: false,
            build: BenchmarkBuild::default(),
            machine: BenchmarkMachine::default(),
            metrics: BenchmarkMetrics::default(),
            stages: vec![BenchmarkStage {
                name: "download".into(),
                seconds: 1.5,
                details: "files=1".into(),
            }],
            sol: vec![],
            samples: vec![],
            stage_marks: vec![],
            log_file: None,
            log_line_count: 0,
        }
    }

    #[test]
    fn summary_lists_stages_and_notes() {
        let text = summary_text(&record(), None);
        assert!(text.starts_with(
            "operation: download
sol: na
sol_kind: reference missing
best_measured: not comparable
"
        ));
        assert!(text.contains("elapsed_s: 1.50"));
        assert!(text.contains("download"));
        assert!(text.contains("notes:\nnote"));
    }

    #[test]
    fn zip_contains_record_log_and_images() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("b.zip");
        write_zip(
            &dest,
            &record(),
            None,
            Some("[x] line one\n"),
            &[ExportImage {
                file_name: "rates.png".into(),
                png: vec![1, 2, 3],
            }],
        )
        .unwrap();
        let mut archive = zip::ZipArchive::new(std::fs::File::open(&dest).unwrap()).unwrap();
        let names: Vec<String> = (0..archive.len())
            .map(|i| archive.by_index(i).unwrap().name().to_owned())
            .collect();
        assert_eq!(
            names,
            vec![
                "benchmark.json",
                "summary.txt",
                "benchmark.log",
                "charts/rates.png"
            ]
        );
        let mut log = String::new();
        archive
            .by_name("benchmark.log")
            .unwrap()
            .read_to_string(&mut log)
            .unwrap();
        assert_eq!(log, "[x] line one\n");
    }
}
