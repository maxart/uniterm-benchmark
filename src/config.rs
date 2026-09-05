use crate::model::{AssuranceFinding, AssuranceStatus, ProfileRecord};
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    pub comparison: ComparisonConfig,
    pub contenders: Vec<ContenderConfig>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ComparisonConfig {
    #[serde(default = "default_title")]
    pub title: String,
    #[serde(default)]
    pub seed: u64,
    #[serde(default)]
    pub keep_workdirs: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ContenderConfig {
    pub id: String,
    pub display_name: String,
    pub adapter: String,
    pub binary: PathBuf,
    pub source: PathBuf,
    #[serde(default)]
    pub environment: BTreeMap<String, String>,
    #[serde(default)]
    pub assurance: Vec<AssuranceConfig>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AssuranceConfig {
    pub category: String,
    pub criterion: String,
    pub status: AssuranceStatus,
    #[serde(default = "default_weight")]
    pub weight: f64,
    pub summary: String,
    #[serde(default)]
    pub evidence: Vec<String>,
}

fn default_title() -> String {
    "Terminal multiplexer comparison".to_owned()
}

fn default_weight() -> f64 {
    1.0
}

impl From<&AssuranceConfig> for AssuranceFinding {
    fn from(value: &AssuranceConfig) -> Self {
        Self {
            category: value.category.clone(),
            criterion: value.criterion.clone(),
            status: value.status,
            weight: value.weight,
            summary: value.summary.clone(),
            evidence: value.evidence.clone(),
        }
    }
}

impl Config {
    pub fn load(path: &Path) -> Result<Self, String> {
        let bytes = std::fs::read_to_string(path)
            .map_err(|error| format!("could not read {}: {error}", path.display()))?;
        let mut config: Config = toml::from_str(&bytes)
            .map_err(|error| format!("invalid config {}: {error}", path.display()))?;
        let base = path.parent().unwrap_or_else(|| Path::new("."));
        for contender in &mut config.contenders {
            contender.binary = resolve_path(base, &contender.binary);
            contender.source = resolve_path(base, &contender.source);
            if contender.binary.exists() {
                contender.binary = std::fs::canonicalize(&contender.binary).map_err(|error| {
                    format!(
                        "could not canonicalize {}: {error}",
                        contender.binary.display()
                    )
                })?;
            }
            if contender.source.exists() {
                contender.source = std::fs::canonicalize(&contender.source).map_err(|error| {
                    format!(
                        "could not canonicalize {}: {error}",
                        contender.source.display()
                    )
                })?;
            }
        }
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.comparison.keep_workdirs {
            return Err("raw workdirs cannot be retained".into());
        }
        if self.contenders.len() < 2 {
            return Err("at least two contenders are required".into());
        }
        let mut ids = BTreeSet::new();
        for contender in &self.contenders {
            if contender.id.is_empty()
                || !contender
                    .id
                    .chars()
                    .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
            {
                return Err(format!(
                    "contender id {:?} must contain only ASCII letters, digits, '-' or '_'",
                    contender.id
                ));
            }
            if !ids.insert(&contender.id) {
                return Err(format!("duplicate contender id {:?}", contender.id));
            }
            if !matches!(contender.adapter.as_str(), "uniterm" | "herdr" | "tmux") {
                return Err(format!(
                    "unsupported adapter {:?} for {} (expected uniterm, herdr, or tmux)",
                    contender.adapter, contender.id
                ));
            }
            if !contender.binary.is_file() {
                return Err(format!(
                    "binary for {} does not exist: {}",
                    contender.id,
                    contender.binary.display()
                ));
            }
            if !contender.source.is_dir() {
                return Err(format!(
                    "source for {} does not exist: {}",
                    contender.id,
                    contender.source.display()
                ));
            }
            for item in &contender.assurance {
                if item.weight <= 0.0 || !item.weight.is_finite() {
                    return Err(format!(
                        "assurance weight for {} / {} must be positive",
                        item.category, item.criterion
                    ));
                }
            }
        }

        let baseline: BTreeSet<_> = self.contenders[0]
            .assurance
            .iter()
            .map(|item| (&item.category, &item.criterion))
            .collect();
        for contender in &self.contenders[1..] {
            let actual: BTreeSet<_> = contender
                .assurance
                .iter()
                .map(|item| (&item.category, &item.criterion))
                .collect();
            if actual != baseline {
                return Err(format!(
                    "{} does not use exactly the same assurance criteria as {}",
                    contender.id, self.contenders[0].id
                ));
            }
        }
        Ok(())
    }
}

fn resolve_path(base: &Path, value: &Path) -> PathBuf {
    let expanded = expand_home(value);
    if expanded.is_absolute() {
        expanded
    } else {
        base.join(expanded)
    }
}

fn expand_home(value: &Path) -> PathBuf {
    let text = value.to_string_lossy();
    if text == "~" {
        return std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| value.to_path_buf());
    }
    if let Some(rest) = text.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(rest);
        }
    }
    value.to_path_buf()
}

pub fn profile(name: &str) -> Result<ProfileRecord, String> {
    let profile = match name {
        "smoke" => ProfileRecord {
            name: name.into(),
            startup_iterations: 2,
            command_iterations: 4,
            latency_iterations: 5,
            output_iterations: 2,
            idle_seconds: 2.0,
            settle_seconds: 0.5,
            sample_interval_ms: 100,
            terminal_cols: 120,
            terminal_rows: 40,
            output_lines: 1_000,
            pane_count: 4,
            resize_iterations: 5,
            extra_clients: 2,
        },
        "standard" => ProfileRecord {
            name: name.into(),
            startup_iterations: 8,
            command_iterations: 20,
            latency_iterations: 30,
            output_iterations: 5,
            idle_seconds: 30.0,
            settle_seconds: 3.0,
            sample_interval_ms: 200,
            terminal_cols: 160,
            terminal_rows: 50,
            output_lines: 10_000,
            pane_count: 8,
            resize_iterations: 20,
            extra_clients: 2,
        },
        "marketing" => ProfileRecord {
            name: name.into(),
            startup_iterations: 20,
            command_iterations: 50,
            latency_iterations: 100,
            output_iterations: 10,
            idle_seconds: 300.0,
            settle_seconds: 10.0,
            sample_interval_ms: 500,
            terminal_cols: 160,
            terminal_rows: 50,
            output_lines: 50_000,
            pane_count: 16,
            resize_iterations: 40,
            extra_clients: 2,
        },
        _ => {
            return Err(format!(
                "unknown profile {name:?}; use smoke, standard, or marketing"
            ))
        }
    };
    Ok(profile)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn optional_tmux_fragment_preserves_the_canonical_rubric() {
        let source = concat!(
            include_str!("../comparison.toml"),
            "\n",
            include_str!("../tmux.contender.toml")
        );
        let mut config: Config = toml::from_str(source).unwrap();
        assert_eq!(config.contenders.len(), 3);
        assert_eq!(config.contenders[2].adapter, "tmux");
        for contender in &mut config.contenders {
            contender.binary = std::env::current_exe().unwrap();
            contender.source = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        }
        config.validate().unwrap();
        config.contenders[2].adapter = "unsupported".into();
        assert!(config
            .validate()
            .unwrap_err()
            .contains("unsupported adapter"));
    }

    #[test]
    fn profiles_increase_in_rigor() {
        let smoke = profile("smoke").unwrap();
        let marketing = profile("marketing").unwrap();
        assert!(marketing.idle_seconds > smoke.idle_seconds);
        assert!(marketing.latency_iterations > smoke.latency_iterations);
        assert!(marketing.output_iterations > smoke.output_iterations);
    }

    #[test]
    fn home_expansion_is_conservative() {
        assert_eq!(
            expand_home(Path::new("relative/file")),
            Path::new("relative/file")
        );
    }
}
