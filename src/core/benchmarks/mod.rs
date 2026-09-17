//! Saved performance benchmarks of user-triggered repository actions.

pub mod best;
pub mod export;
pub mod index;
pub mod log_slice;
pub mod record;
pub mod sol;
pub mod store;

pub use best::{BestComparison, best_comparable};
pub use record::{
    BENCHMARK_RECORD_VERSION, BenchmarkBuild, BenchmarkKind, BenchmarkMachine, BenchmarkMetrics,
    BenchmarkOutcome, BenchmarkRecord, BenchmarkRepository, BenchmarkSample, BenchmarkStageMark,
    downsample_samples,
};
pub use sol::{SolDetailValue, SolMetricKind, SolOpSummary};
