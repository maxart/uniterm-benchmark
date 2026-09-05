use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const RESULT_SCHEMA_VERSION: u32 = 7;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunReport {
    pub schema_version: u32,
    pub tool_version: String,
    pub run_id: String,
    #[serde(default, skip_serializing)]
    pub started_unix_ms: u128,
    pub host: HostInfo,
    pub profile: ProfileRecord,
    pub fairness: FairnessRecord,
    pub contenders: Vec<ContenderResult>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostInfo {
    pub os: String,
    pub architecture: String,
    pub kernel: String,
    pub hostname: String,
    pub logical_cpus: usize,
    pub rustc: Option<String>,
    pub wsl: bool,
    pub git_dirty_policy: String,
    #[serde(default)]
    pub cpu_time_source: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileRecord {
    pub name: String,
    pub startup_iterations: usize,
    pub command_iterations: usize,
    pub latency_iterations: usize,
    pub output_iterations: usize,
    pub idle_seconds: f64,
    pub settle_seconds: f64,
    pub sample_interval_ms: u64,
    pub terminal_cols: u16,
    pub terminal_rows: u16,
    pub output_lines: usize,
    pub pane_count: usize,
    /// Rapid window resizes applied to the attached client during the resize storm.
    pub resize_iterations: usize,
    /// Additional clients attached beside the primary client in the multi-client scenario.
    pub extra_clients: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FairnessRecord {
    pub run_order: Vec<String>,
    pub release_binaries_required: bool,
    pub network_disabled_during_benchmarks: bool,
    pub isolated_home_and_xdg: bool,
    pub identical_terminal_geometry: bool,
    pub child_workload: String,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContenderResult {
    pub id: String,
    pub display_name: String,
    pub adapter: String,
    pub binary: ArtifactInfo,
    pub source: SourceInfo,
    pub static_analysis: StaticAnalysis,
    pub assurance: Vec<AssuranceFinding>,
    pub benchmarks: Vec<BenchmarkResult>,
    pub errors: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtifactInfo {
    pub path: String,
    pub bytes: u64,
    pub version_output: String,
    pub sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceInfo {
    pub path: String,
    pub commit: Option<String>,
    pub commit_date: Option<String>,
    pub dirty: Option<bool>,
    pub package_version: Option<String>,
    pub license: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StaticAnalysis {
    pub rust_source_files: Option<u64>,
    pub first_party_rust_lines: Option<u64>,
    pub rust_test_lines: Option<u64>,
    pub documentation_lines: u64,
    pub lockfile_packages: Option<u64>,
    pub direct_dependencies: Option<u64>,
    pub unsafe_blocks: Option<u64>,
    pub production_unwrap_calls: Option<u64>,
    pub network_api_references: Option<u64>,
    pub process_launch_references: Option<u64>,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssuranceFinding {
    pub category: String,
    pub criterion: String,
    pub status: AssuranceStatus,
    pub weight: f64,
    pub summary: String,
    pub evidence: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssuranceStatus {
    Pass,
    Partial,
    Fail,
    Unknown,
    NotApplicable,
}

impl AssuranceStatus {
    pub fn score(self) -> Option<f64> {
        match self {
            Self::Pass => Some(100.0),
            Self::Partial => Some(50.0),
            Self::Fail => Some(0.0),
            Self::Unknown | Self::NotApplicable => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchmarkResult {
    pub name: String,
    pub unit: String,
    pub direction: MetricDirection,
    pub samples: Vec<f64>,
    pub summary: SampleSummary,
    pub status: BenchmarkStatus,
    pub note: String,
    pub metadata: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MetricDirection {
    Lower,
    Higher,
    Neutral,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BenchmarkStatus {
    Measured,
    Skipped,
    Failed,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SampleSummary {
    pub count: usize,
    pub min: Option<f64>,
    pub median: Option<f64>,
    pub mean: Option<f64>,
    pub p95: Option<f64>,
    pub max: Option<f64>,
    pub stddev: Option<f64>,
}

impl SampleSummary {
    pub fn from_samples(values: &[f64]) -> Self {
        let mut finite: Vec<f64> = values.iter().copied().filter(|v| v.is_finite()).collect();
        if finite.is_empty() {
            return Self::default();
        }
        finite.sort_by(f64::total_cmp);
        let count = finite.len();
        let mean = finite.iter().sum::<f64>() / count as f64;
        let variance = finite
            .iter()
            .map(|value| {
                let delta = value - mean;
                delta * delta
            })
            .sum::<f64>()
            / count as f64;
        Self {
            count,
            min: finite.first().copied(),
            median: Some(percentile(&finite, 0.5)),
            mean: Some(mean),
            p95: Some(percentile(&finite, 0.95)),
            max: finite.last().copied(),
            stddev: Some(variance.sqrt()),
        }
    }
}

fn percentile(sorted: &[f64], quantile: f64) -> f64 {
    if sorted.len() == 1 {
        return sorted[0];
    }
    let position = quantile.clamp(0.0, 1.0) * (sorted.len() - 1) as f64;
    let lower = position.floor() as usize;
    let upper = position.ceil() as usize;
    if lower == upper {
        sorted[lower]
    } else {
        let fraction = position - lower as f64;
        sorted[lower] * (1.0 - fraction) + sorted[upper] * fraction
    }
}

pub fn measured_benchmark(
    name: impl Into<String>,
    unit: impl Into<String>,
    direction: MetricDirection,
    samples: Vec<f64>,
    note: impl Into<String>,
) -> BenchmarkResult {
    let summary = SampleSummary::from_samples(&samples);
    BenchmarkResult {
        name: name.into(),
        unit: unit.into(),
        direction,
        samples,
        summary,
        status: BenchmarkStatus::Measured,
        note: note.into(),
        metadata: BTreeMap::new(),
    }
}

pub fn failed_benchmark(name: impl Into<String>, note: impl Into<String>) -> BenchmarkResult {
    BenchmarkResult {
        name: name.into(),
        unit: String::new(),
        direction: MetricDirection::Neutral,
        samples: Vec::new(),
        summary: SampleSummary::default(),
        status: BenchmarkStatus::Failed,
        note: note.into(),
        metadata: BTreeMap::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summaries_are_stable_and_interpolated() {
        let summary = SampleSummary::from_samples(&[4.0, 1.0, 3.0, 2.0]);
        assert_eq!(summary.count, 4);
        assert_eq!(summary.min, Some(1.0));
        assert_eq!(summary.median, Some(2.5));
        assert_eq!(summary.max, Some(4.0));
        assert!((summary.p95.unwrap() - 3.85).abs() < 0.0001);
    }

    #[test]
    fn assurance_unknown_is_not_scored() {
        assert_eq!(AssuranceStatus::Pass.score(), Some(100.0));
        assert_eq!(AssuranceStatus::Unknown.score(), None);
    }
}
