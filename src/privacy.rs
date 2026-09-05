//! Output allowlist. Never pass through product diagnostics or operator-authored text.
use crate::model::{BenchmarkStatus, RunReport, RESULT_SCHEMA_VERSION};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

pub const TITLE: &str = "Terminal multiplexer comparison";
const OMITTED: &str = "Diagnostic details omitted for privacy.";

/// Keep only the leading release number, never a build path, name, date, or arbitrary suffix.
pub fn version(value: &str) -> Option<String> {
    let value = value.split_whitespace().next()?;
    let numeric: String = value
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    let parts: Vec<_> = numeric.split('.').collect();
    if !(2..=3).contains(&parts.len()) || parts.iter().any(|p| p.is_empty() || p.len() > 6) {
        return None;
    }
    // tmux release suffixes are a single lower-case letter (for example 3.7c).
    let suffix = &value[numeric.len()..];
    if parts.len() == 2 && suffix.len() == 1 && suffix.as_bytes()[0].is_ascii_lowercase() {
        return Some(format!("{numeric}{suffix}"));
    }
    if suffix.is_empty() {
        Some(numeric)
    } else {
        Some(format!("{numeric}-redacted"))
    }
}

pub fn product_version(adapter: &str, output: &str) -> String {
    let mut words = output.split_whitespace();
    let product = words.next().unwrap_or_default();
    let expected = match adapter {
        "uniterm" => "uniterm",
        "herdr" => "herdr",
        "tmux" => "tmux",
        _ => "unknown",
    };
    let release = if product == expected {
        words.next().and_then(version)
    } else {
        None
    };
    format!("{expected} {}", release.as_deref().unwrap_or("unknown"))
}

/// Deliberately fixed messages: regex replacement cannot reliably sanitize arbitrary stderr.
pub fn public_error(error: &str) -> String {
    let category = if error.starts_with("could not read") {
        "could not read input; check the supplied file and permissions"
    } else if error.starts_with("invalid config") {
        "invalid configuration; check the TOML structure and documented fields"
    } else if error.starts_with("could not write")
        || error.starts_with("could not replace")
        || error.starts_with("could not create")
    {
        "could not write output; check the destination and permissions"
    } else if error.starts_with("measurement failures") {
        "measurement failures were recorded; inspect the sanitized report"
    } else if error.starts_with("unsupported schema") {
        "unsupported report schema"
    } else if error.contains("cannot be merged") || error.contains("different adapter") {
        "reports are not comparable and cannot be merged"
    } else if error.contains("workdirs") {
        "raw work directories cannot be retained in sanitized mode"
    } else {
        "operation failed; check the command, configuration, prerequisites, and measurement status"
    };
    format!("{category}. {OMITTED}")
}

fn hex(value: &str) -> bool {
    matches!(value.len(), 40 | 64) && value.bytes().all(|b| b.is_ascii_hexdigit())
}

fn adapter(value: &str) -> Result<&'static str, String> {
    match value {
        "uniterm" => Ok("uniterm"),
        "herdr" => Ok("herdr"),
        "tmux" => Ok("tmux"),
        _ => Err("unsupported report adapter".into()),
    }
}

fn known_metric(name: &str) -> bool {
    const SIMPLE: &[&str] = &[
        "binary_size",
        "client_render_output",
        "control_command_latency",
        "daemon_idle",
        "foreground_idle",
        "isolated_state_size",
        "live_suite_shutdown",
        "multipane_idle",
        "multipane_process_count",
        "multipane_suite_shutdown",
        "pane_close_recovery",
        "pane_close_rss",
        "pane_memory_slope",
        "resize_storm_cpu",
        "resize_storm_rss",
        "resize_storm_settle",
        "restart_ready",
        "server_shutdown",
        "server_startup_ready",
        "terminal_input_to_visible",
        "terminal_output_completion",
        "terminal_output_ingest_rate",
    ];
    if SIMPLE.contains(&name) {
        return true;
    }
    for prefix in ["daemon_idle", "foreground_idle"] {
        if name.strip_prefix(prefix).is_some_and(resource_suffix) {
            return true;
        }
    }
    for prefix in ["multipane_", "multiclient_"] {
        if let Some(rest) = name.strip_prefix(prefix) {
            if let Some((count, suffix)) = rest.split_once('_') {
                if !count.is_empty()
                    && count.len() <= 4
                    && count.bytes().all(|b| b.is_ascii_digit())
                    && (suffix == "input_to_visible"
                        || suffix == "idle"
                        || suffix.strip_prefix("idle").is_some_and(resource_suffix))
                {
                    return true;
                }
            }
        }
    }
    false
}

fn resource_suffix(suffix: &str) -> bool {
    matches!(
        suffix,
        "_root_cpu" | "_cohort_cpu" | "_root_rss" | "_cohort_rss"
    )
}

fn safe_metadata(key: &str, value: &str) -> bool {
    match key {
        "native_disk_restoration" => value == "not_applicable",
        "prior_output_visible_after_restart" => matches!(value, "yes" | "no" | "unknown"),
        "pane_size_rows_cols" => {
            let parts: Vec<_> = value.split(' ').collect();
            parts.len() == 2 && parts.iter().all(|p| p.parse::<u16>().is_ok())
        }
        "cohort_rss_kib_by_pane_count" => value.split(',').all(|pair| {
            pair.split_once(':').is_some_and(|(panes, rss)| {
                panes.parse::<u16>().is_ok() && rss.parse::<u64>().is_ok()
            })
        }),
        "one_pane_rss_kib"
        | "peak_rss_kib"
        | "after_close_rss_kib"
        | "exit_retries"
        | "input_lines_per_iteration"
        | "output_iterations"
        | "input_bytes_per_iteration"
        | "resize_iterations"
        | "settle_probes"
        | "scrollback_lines_before_storm"
        | "pane_shells_after_restart" => {
            !value.is_empty()
                && value.len() <= 20
                && value.bytes().all(|b| b.is_ascii_digit())
                && value.parse::<u64>().is_ok()
        }
        _ => false,
    }
}

const CRITERIA: &[(&str, &str)] = &[
    ("security", "Memory-safety boundary"),
    ("security", "Local IPC access control"),
    ("security", "Untrusted protocol input bounds"),
    ("security", "Crash-safe persistence"),
    ("security", "Update integrity"),
    ("security", "Extension trust boundary"),
    ("security", "Remote exposure and authentication"),
    ("security", "Child-process containment"),
    ("privacy", "Telemetry and analytics"),
    ("privacy", "Default outbound network activity"),
    ("privacy", "Local data retention"),
    ("privacy", "Clipboard and host-data access"),
    ("privacy", "Remote data path"),
    ("privacy", "Network controls"),
];

/// Produce a new report without identities, paths, arbitrary text, or terminal contents.
/// Numeric measurements, statuses, source commits and artifact hashes remain unchanged.
/// Repeated application is idempotent. Unknown fields/metrics fail closed or are omitted.
pub fn sanitize(report: &RunReport) -> Result<RunReport, String> {
    let mut out = report.clone();
    out.schema_version = RESULT_SCHEMA_VERSION;
    out.started_unix_ms = 0;
    // A one-way opaque identifier preserves reproducibility without publishing the wall clock.
    if !(out.run_id.starts_with("run-") && out.run_id.len() == 68 && hex(&out.run_id[4..])) {
        out.run_id = format!("run-{:x}", Sha256::digest(report.run_id.as_bytes()));
    }
    out.tool_version = version(&out.tool_version).unwrap_or_else(|| "unknown".into());
    out.host.hostname = "host".into();
    out.host.os = match out.host.os.as_str() {
        "linux" => "linux",
        "macos" => "macos",
        _ => "unknown",
    }
    .into();
    out.host.architecture = match out.host.architecture.as_str() {
        "x86_64" => "x86_64",
        "aarch64" => "aarch64",
        "x86" => "x86",
        "arm" => "arm",
        _ => "unknown",
    }
    .into();
    let kernel = out
        .host
        .kernel
        .split_whitespace()
        .nth(1)
        .and_then(version)
        .unwrap_or_else(|| "unknown".into());
    out.host.kernel = format!(
        "{} {kernel}",
        if out.host.os == "macos" {
            "Darwin"
        } else if out.host.os == "linux" {
            "Linux"
        } else {
            "unknown"
        }
    );
    out.host.rustc = out
        .host
        .rustc
        .as_deref()
        .and_then(|v| v.strip_prefix("rustc "))
        .and_then(version)
        .map(|v| format!("rustc {v}"));
    out.host.git_dirty_policy = "Source dirtiness is recorded; local paths are omitted.".into();
    out.host.cpu_time_source = if out.host.cpu_time_source.starts_with("/proc/<pid>/stat") {
        // Retain the measured tick resolution, not arbitrary suffix text from imported reports.
        let ticks = out
            .host
            .cpu_time_source
            .split('(')
            .nth(1)
            .and_then(|s| s.split_whitespace().next())
            .and_then(|s| s.parse::<u32>().ok());
        match ticks {
            Some(n) => {
                format!("/proc/<pid>/stat ({n} ms ticks); ps supplies RSS and process ancestry")
            }
            None => "proc cumulative CPU time".into(),
        }
    } else if out.host.cpu_time_source.starts_with("proc cumulative") {
        "proc cumulative CPU time".into()
    } else if out.host.cpu_time_source.starts_with("ps ") {
        "ps cumulative CPU time".into()
    } else {
        "unknown".into()
    };
    if !matches!(
        out.profile.name.as_str(),
        "smoke" | "standard" | "marketing"
    ) {
        return Err("unsupported benchmark profile".into());
    }
    out.fairness.child_workload = format!(
        "POSIX shell, {}x{}, {} deterministic output lines",
        out.profile.terminal_cols, out.profile.terminal_rows, out.profile.output_lines
    );
    out.fairness.notes = vec![
        "Equivalent workloads use isolated HOME/XDG trees and real PTYs; product commands only set up, probe, and stop sessions.".into(),
        "Startup order reverses; suite order rotates by seed. Latency/output screen checks gate validity. Root and cohort metrics are separate.".into(),
        "Herdr background checks are disabled and its headless geometry is pinned. tmux uses a private socket/config, a non-login shell and tiled prefix-key splits.".into(),
        "Output privacy: host identities, paths, wall-clock timestamps, custom text, raw diagnostics and terminal contents are omitted. Counts, measurements and revision/artifact hashes are retained.".into(),
    ];
    out.warnings = out
        .warnings
        .iter()
        .map(|_| "A run warning occurred; identifying details omitted.".into())
        .collect();
    // Sort original IDs consistently, with numeric ordering for existing aliases so
    // sanitizing ten or more instances of the same adapter remains idempotent.
    let mut identities: Vec<_> = report
        .contenders
        .iter()
        .map(|c| (&c.id, &c.adapter))
        .collect();
    identities.sort_by_key(|(id, name)| {
        let rank = if id == name {
            Some(1)
        } else {
            id.strip_prefix(name.as_str())
                .and_then(|s| s.strip_prefix('-'))
                .and_then(|s| s.parse::<usize>().ok())
                .filter(|n| *n >= 2)
        };
        (*name, rank.unwrap_or(usize::MAX), *id)
    });
    let mut counts = BTreeMap::new();
    let mut aliases = BTreeMap::new();
    for (id, name) in identities {
        let name = adapter(name)?;
        let count = counts.entry(name).or_insert(0);
        *count += 1;
        let previous = aliases.insert(
            id.clone(),
            if *count == 1 {
                name.into()
            } else {
                format!("{name}-{count}")
            },
        );
        if previous.is_some() {
            return Err("duplicate contender identity".into());
        }
    }
    out.fairness.run_order = report
        .fairness
        .run_order
        .iter()
        .filter_map(|id| aliases.get(id).cloned())
        .collect();
    for c in &mut out.contenders {
        let product = adapter(&c.adapter)?;
        c.id = aliases
            .get(&c.id)
            .cloned()
            .ok_or("invalid contender identity")?;
        c.display_name = c.id.clone();
        c.binary.path = format!("<{}-binary>", c.id);
        c.binary.version_output = product_version(product, &c.binary.version_output);
        if !hex(&c.binary.sha256) {
            return Err("invalid artifact digest".into());
        }
        c.source.path = format!("<{}-source>", c.id);
        if c.source.commit.as_deref().is_some_and(|v| !hex(v)) {
            return Err("invalid source commit".into());
        }
        c.source.commit_date = None;
        c.source.package_version = c.source.package_version.as_deref().and_then(version);
        c.source.license = c
            .source
            .license
            .as_deref()
            .filter(|v| {
                matches!(
                    *v,
                    "MIT"
                        | "ISC"
                        | "Apache-2.0"
                        | "MIT OR Apache-2.0"
                        | "BSD-2-Clause"
                        | "BSD-3-Clause"
                        | "AGPL-3.0-or-later"
                        | "GPL-3.0-or-later"
                )
            })
            .map(str::to_owned);
        c.static_analysis.notes = vec!["Static counts are context only; generated/vendored code and duplicate/translations are excluded. Rust/Cargo counts are inapplicable to C; Markdown counts exclude man pages.".into()];
        c.assurance
            .retain(|a| CRITERIA.contains(&(a.category.as_str(), a.criterion.as_str())));
        for a in &mut c.assurance {
            a.summary = "Operator-supplied rating; free-text rationale and evidence paths omitted for privacy. The harness does not independently verify this rating.".into();
            a.evidence.clear();
        }
        c.errors = c
            .errors
            .iter()
            .map(|_| {
                "A scenario failed; identifying details omitted. Inspect metric statuses.".into()
            })
            .collect();
        for b in &mut c.benchmarks {
            if !known_metric(&b.name) {
                return Err("unsupported benchmark metric".into());
            }
            if !matches!(
                b.unit.as_str(),
                "" | "ms"
                    | "% core"
                    | "KiB"
                    | "bytes"
                    | "processes"
                    | "KiB/pane"
                    | "%"
                    | "MiB/s"
                    | "ms CPU"
            ) {
                return Err("unsupported metric unit".into());
            }
            b.note = match b.status {
                BenchmarkStatus::Measured => "Measured with the documented profile; see docs/METHODOLOGY.md for interpretation.",
                BenchmarkStatus::Failed => "Measurement failed. Raw diagnostics and terminal output omitted for privacy.",
                BenchmarkStatus::Skipped => "Measurement skipped. Raw diagnostics omitted for privacy.",
            }.into();
            b.metadata.retain(|key, value| safe_metadata(key, value));
        }
    }
    Ok(out)
}
