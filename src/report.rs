use crate::model::{
    AssuranceStatus, BenchmarkResult, BenchmarkStatus, ContenderResult, MetricDirection, RunReport,
    RESULT_SCHEMA_VERSION,
};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::Path;

const CORE_METRICS: &[&str] = &[
    "server_startup_ready",
    "control_command_latency",
    "daemon_idle_cohort_cpu",
    "daemon_idle_cohort_rss",
    "foreground_idle_cohort_cpu",
    "foreground_idle_cohort_rss",
    "terminal_input_to_visible",
    "terminal_output_completion",
];

/// A failed or missing common measurement must never look like a successful CLI run.
pub fn has_failures(report: &RunReport) -> bool {
    report.contenders.iter().any(|c| {
        !c.errors.is_empty()
            || c.benchmarks
                .iter()
                .any(|b| b.status != BenchmarkStatus::Measured)
            || CORE_METRICS
                .iter()
                .any(|name| metric(c, name).and_then(median).is_none())
    })
}

pub fn load_reports(paths: &[impl AsRef<Path>]) -> Result<Vec<RunReport>, String> {
    load_reports_internal(paths).map_err(|error| crate::privacy::public_error(&error))
}

fn load_reports_internal(paths: &[impl AsRef<Path>]) -> Result<Vec<RunReport>, String> {
    let mut reports = Vec::new();
    for path in paths {
        let path = path.as_ref();
        let bytes = std::fs::read(path)
            .map_err(|error| format!("could not read {}: {error}", path.display()))?;
        let report: RunReport = serde_json::from_slice(&bytes)
            .map_err(|error| format!("invalid report {}: {error}", path.display()))?;
        if !matches!(report.schema_version, 5 | 6 | RESULT_SCHEMA_VERSION) {
            return Err(format!(
                "unsupported schema {} in {} (tool supports {})",
                report.schema_version,
                path.display(),
                RESULT_SCHEMA_VERSION
            ));
        }
        reports.push(report);
    }
    validate_merge(&reports)?;
    reports.iter().map(crate::privacy::sanitize).collect()
}

fn validate_merge(reports: &[RunReport]) -> Result<(), String> {
    let Some(first) = reports.first() else {
        return Err("at least one report is required".into());
    };
    let ids: BTreeSet<_> = first.contenders.iter().map(|item| &item.id).collect();
    for report in &reports[1..] {
        if report.schema_version != first.schema_version {
            return Err("runs with different result schemas cannot be merged".into());
        }
        let actual: BTreeSet<_> = report.contenders.iter().map(|item| &item.id).collect();
        if actual != ids {
            return Err(format!(
                "run {} has a different contender set and cannot be merged",
                report.run_id
            ));
        }
        if report.profile.name != first.profile.name
            || report.profile.startup_iterations != first.profile.startup_iterations
            || report.profile.command_iterations != first.profile.command_iterations
            || report.profile.latency_iterations != first.profile.latency_iterations
            || report.profile.output_iterations != first.profile.output_iterations
            || report.profile.settle_seconds != first.profile.settle_seconds
            || report.profile.sample_interval_ms != first.profile.sample_interval_ms
            || report.profile.idle_seconds != first.profile.idle_seconds
            || report.profile.terminal_cols != first.profile.terminal_cols
            || report.profile.terminal_rows != first.profile.terminal_rows
            || report.profile.output_lines != first.profile.output_lines
            || report.profile.pane_count != first.profile.pane_count
            || report.profile.resize_iterations != first.profile.resize_iterations
            || report.profile.extra_clients != first.profile.extra_clients
        {
            return Err(format!(
                "run {} uses a different benchmark profile and cannot be merged",
                report.run_id
            ));
        }
        for baseline in &first.contenders {
            let current = contender(report, &baseline.id).expect("validated contender set");
            if baseline.adapter != current.adapter {
                return Err(format!(
                    "run {} uses a different adapter for {}",
                    report.run_id, baseline.id
                ));
            }
            if baseline.source.commit.is_some()
                && current.source.commit.is_some()
                && baseline.source.commit != current.source.commit
            {
                return Err(format!(
                    "run {} uses a different {} source commit and cannot be merged",
                    report.run_id, baseline.display_name
                ));
            }
        }
    }
    Ok(())
}

pub fn markdown(_title: &str, reports: &[RunReport]) -> Result<String, String> {
    validate_merge(reports).map_err(|error| crate::privacy::public_error(&error))?;
    let sanitized: Vec<_> = reports
        .iter()
        .map(crate::privacy::sanitize)
        .collect::<Result<_, _>>()?;
    let reports = sanitized.as_slice();
    let first = &reports[0];
    let mut out = String::new();
    writeln!(out, "# {}\n", crate::privacy::TITLE).unwrap();
    writeln!(
        out,
        "> Generated by `ut-compare {}` from {} isolated run(s). Results are comparative measurements, not universal constants.\n",
        first.tool_version,
        reports.len()
    )
    .unwrap();

    writeln!(out, "## Executive summary\n").unwrap();
    for report in reports {
        let scores = performance_scores(report);
        let mut ranked: Vec<_> = scores.iter().collect();
        ranked.sort_by(|left, right| right.1.total_cmp(left.1));
        let host_label = host_label(report);
        if let Some((winner, score)) = ranked.first().filter(|item| *item.1 > 0.0) {
            writeln!(
                out,
                "- **{}:** {} ranks first on the balanced performance index ({:.1}/100). The index normalizes only the core measured metrics listed below; inspect the raw table before making a claim.",
                host_label,
                display_name(report, winner),
                score
            )
            .unwrap();
        } else {
            writeln!(
                out,
                "- **{}:** no balanced performance ranking is available because no complete core metric set was measured.",
                host_label
            )
            .unwrap();
        }
    }
    let assurance = assurance_scores(&first.contenders);
    for (category, values) in assurance {
        let mut ranking: Vec<_> = values.into_iter().collect();
        ranking.sort_by(|left, right| right.1.total_cmp(&left.1));
        if let Some((id, score)) = ranking.first() {
            writeln!(
                out,
                "- **{} assurance:** {} has the highest evidence-weighted checklist score ({:.1}/100). This is a transparent review rubric, not a penetration test or certification.",
                title_case(&category),
                display_name(first, id),
                score
            )
            .unwrap();
        }
    }
    writeln!(out).unwrap();

    writeln!(out, "## Test identity and comparability\n").unwrap();
    writeln!(
        out,
        "| Host | Profile | Kernel | Architecture | WSL | Contender revisions |"
    )
    .unwrap();
    writeln!(out, "| --- | --- | --- | --- | --- | --- |").unwrap();
    for report in reports {
        let revisions = report
            .contenders
            .iter()
            .map(|item| {
                format!(
                    "{} `{}`{}",
                    item.display_name,
                    item.source
                        .commit
                        .as_deref()
                        .map(|commit| &commit[..commit.len().min(12)])
                        .unwrap_or("unknown"),
                    if item.source.dirty == Some(true) {
                        " (dirty)"
                    } else {
                        ""
                    }
                )
            })
            .collect::<Vec<_>>()
            .join("<br>");
        writeln!(
            out,
            "| {} | {} | {} | {} | {} | {} |",
            escape_cell(&host_label(report)),
            report.profile.name,
            escape_cell(&report.host.kernel),
            report.host.architecture,
            if report.host.wsl { "yes" } else { "no" },
            revisions
        )
        .unwrap();
    }
    writeln!(out).unwrap();

    writeln!(out, "The harness enforces these controls:\n").unwrap();
    for note in &first.fairness.notes {
        writeln!(out, "- {note}").unwrap();
    }
    writeln!(
        out,
        "- Geometry is {}x{}; the live workload uses {} panes and {} deterministic output lines.",
        first.profile.terminal_cols,
        first.profile.terminal_rows,
        first.profile.pane_count,
        first.profile.output_lines
    )
    .unwrap();
    writeln!(out).unwrap();

    writeln!(out, "## Performance results\n").unwrap();
    for report in reports {
        writeln!(out, "### {}\n", host_label(report)).unwrap();
        write_performance_index(&mut out, report);
        write_metric_table(&mut out, report);
        write_restoration_context(&mut out, report);
        if !report.warnings.is_empty() {
            writeln!(out, "Warnings:\n").unwrap();
            for warning in &report.warnings {
                writeln!(out, "- {warning}").unwrap();
            }
            writeln!(out).unwrap();
        }
    }

    writeln!(out, "## Security and privacy assessment\n").unwrap();
    writeln!(
        out,
        "Ratings are operator supplied and are not independently verified. Free-text rationale and evidence paths are withheld by the privacy filter. Checklist scores use pass=100, partial=50, fail=0. Unknown and not-applicable items are excluded, and every criterion has the same definition for every contender. Static code counts do not affect these scores.\n"
    )
    .unwrap();
    write_assurance_summary(&mut out, first);
    write_assurance_details(&mut out, first);

    writeln!(out, "## Static engineering context\n").unwrap();
    writeln!(out, "| Context metric | {} |", contender_headers(first)).unwrap();
    writeln!(
        out,
        "| --- | {} |",
        vec!["---:"; first.contenders.len()].join(" | ")
    )
    .unwrap();
    static_row(&mut out, first, "First-party Rust lines", |c| {
        c.static_analysis.first_party_rust_lines
    });
    static_row(&mut out, first, "Rust test lines", |c| {
        c.static_analysis.rust_test_lines
    });
    static_row(&mut out, first, "Markdown documentation lines", |c| {
        Some(c.static_analysis.documentation_lines)
    });
    static_row(&mut out, first, "Cargo.lock packages", |c| {
        c.static_analysis.lockfile_packages
    });
    static_row(&mut out, first, "Direct Cargo dependencies", |c| {
        c.static_analysis.direct_dependencies
    });
    static_row(&mut out, first, "Lexical unsafe blocks", |c| {
        c.static_analysis.unsafe_blocks
    });
    static_row(&mut out, first, "Lexical production unwrap calls", |c| {
        c.static_analysis.production_unwrap_calls
    });
    writeln!(out).unwrap();
    writeln!(
        out,
        "These counts describe implementation surface, not product quality. Herdr's vendored Ghostty implementation and generated bindings are excluded; Uniterm's external crates are also represented only through lockfile counts.\n"
    )
    .unwrap();

    if first.contenders.iter().any(|c| c.adapter == "tmux") {
        writeln!(out, "Rust/Cargo-specific counts are N/A for tmux's C implementation. This collector does not count C dependencies or man-page documentation; N/A does not imply zero unsafe code or zero dependencies.\n").unwrap();
    }

    writeln!(out, "## Claim guidance and limitations\n").unwrap();
    writeln!(out, "Safe marketing use requires all of the following:\n").unwrap();
    writeln!(out, "- Publish the raw JSON, tool revision, app revisions, profile, host details, and exact binaries or hashes.").unwrap();
    writeln!(out, "- Say \"in our measured workload\" and name the OS/hardware. Do not generalize one machine to every installation.").unwrap();
    writeln!(out, "- Use the `marketing` profile for public CPU claims; smoke results validate the harness but are too short for stable idle-CPU claims.").unwrap();
    writeln!(out, "- Repeat on native Linux, WSL2, Intel/Apple Silicon macOS as relevant, with at least three runs per host and alternating product order.").unwrap();
    writeln!(out, "- Do not describe the assurance checklist as an audit, certification, exploit test, or guarantee.").unwrap();
    writeln!(out, "- Re-run after either binary, configuration, dependency lockfile, or benchmark tool changes.").unwrap();
    writeln!(out).unwrap();
    writeln!(out, "Known limitations:\n").unwrap();
    writeln!(out, "- PTY latency intentionally includes the same `/bin/sh` scheduling and rendering path for both products; it is end-to-end, not an internal parser microbenchmark.").unwrap();
    writeln!(out, "- RSS is sampled from `ps`; allocator-resident versus reclaimable memory is not separated.").unwrap();
    writeln!(
        out,
        "- CPU resolution and background system noise limit very short runs."
    )
    .unwrap();
    writeln!(out, "- Feature breadth is not folded into the performance score. A faster result does not imply feature parity.").unwrap();
    writeln!(out, "- Manual assurance findings are point-in-time source review and must be revisited when implementations change.").unwrap();

    Ok(out)
}

fn write_performance_index(out: &mut String, report: &RunReport) {
    let scores = performance_scores(report);
    let mut ranked: Vec<_> = scores.into_iter().collect();
    ranked.sort_by(|left, right| right.1.total_cmp(&left.1));
    writeln!(
        out,
        "| Rank | Contender | Balanced index | Core metrics present |"
    )
    .unwrap();
    writeln!(out, "| ---: | --- | ---: | ---: |").unwrap();
    for (rank, (id, score)) in ranked.iter().enumerate() {
        let contender = contender(report, id).unwrap();
        let present = CORE_METRICS
            .iter()
            .filter(|name| metric(contender, name).and_then(median).is_some())
            .count();
        writeln!(
            out,
            "| {} | {} | {:.1} | {}/{} |",
            rank + 1,
            contender.display_name,
            score,
            present,
            CORE_METRICS.len()
        )
        .unwrap();
    }
    writeln!(out).unwrap();
}

fn write_metric_table(out: &mut String, report: &RunReport) {
    let names = ordered_metric_names(report);
    writeln!(out, "| Metric | {} | Winner |", contender_headers(report)).unwrap();
    writeln!(
        out,
        "| --- | {} | --- |",
        vec!["---:"; report.contenders.len()].join(" | ")
    )
    .unwrap();
    for name in names {
        let exemplar = report
            .contenders
            .iter()
            .find_map(|contender| metric(contender, &name));
        let unit = exemplar.map(|metric| metric.unit.as_str()).unwrap_or("");
        let direction = exemplar
            .map(|metric| metric.direction)
            .unwrap_or(MetricDirection::Neutral);
        let values: Vec<Option<f64>> = report
            .contenders
            .iter()
            .map(|contender| metric(contender, &name).and_then(median))
            .collect();
        let winner = winner_label(report, &values, direction);
        let cells = report
            .contenders
            .iter()
            .zip(&values)
            .map(|(contender, value)| metric_cell(raw_metric(contender, &name), *value, unit))
            .collect::<Vec<_>>()
            .join(" | ");
        writeln!(out, "| `{}` ({}) | {} | {} |", name, unit, cells, winner).unwrap();
    }
    writeln!(out).unwrap();
}

fn metric_cell(metric: Option<&BenchmarkResult>, value: Option<f64>, unit: &str) -> String {
    if let Some(value) = value {
        return format_value(value, unit);
    }
    match metric.map(|metric| metric.status) {
        Some(BenchmarkStatus::Failed) => "FAILED".into(),
        Some(BenchmarkStatus::Skipped) => "skipped".into(),
        _ => "missing".into(),
    }
}

fn restoration_cell(contender: &ContenderResult) -> &str {
    let Some(restart) = raw_metric(contender, "restart_ready") else {
        return "missing";
    };
    if restart.status != BenchmarkStatus::Measured {
        return "FAILED";
    }
    if restart
        .metadata
        .get("native_disk_restoration")
        .map(String::as_str)
        == Some("not_applicable")
    {
        return "N/A (no native disk restoration)";
    }
    restart
        .metadata
        .get("prior_output_visible_after_restart")
        .map(String::as_str)
        .unwrap_or("unknown")
}

fn write_restoration_context(out: &mut String, report: &RunReport) {
    writeln!(out, "| Feature context | {} |", contender_headers(report)).unwrap();
    writeln!(
        out,
        "| --- | {} |",
        vec!["---"; report.contenders.len()].join(" | ")
    )
    .unwrap();
    writeln!(
        out,
        "| Prior output after graceful server restart | {} |\n",
        report
            .contenders
            .iter()
            .map(restoration_cell)
            .collect::<Vec<_>>()
            .join(" | ")
    )
    .unwrap();
    writeln!(out, "Restoration is context only. tmux restart readiness measures creation of a fresh session; disk bytes remain a measured footprint, not a persistence-quality score. Missing or failed common workloads are never N/A.\n").unwrap();
}

fn write_assurance_summary(out: &mut String, report: &RunReport) {
    let scores = assurance_scores(&report.contenders);
    writeln!(out, "| Category | {} |", contender_headers(report)).unwrap();
    writeln!(
        out,
        "| --- | {} |",
        vec!["---:"; report.contenders.len()].join(" | ")
    )
    .unwrap();
    for (category, values) in scores {
        let cells = report
            .contenders
            .iter()
            .map(|contender| {
                values
                    .get(&contender.id)
                    .map(|score| format!("{score:.1}"))
                    .unwrap_or_else(|| "n/a".into())
            })
            .collect::<Vec<_>>()
            .join(" | ");
        writeln!(out, "| {} | {} |", title_case(&category), cells).unwrap();
    }
    writeln!(out).unwrap();
}

fn write_assurance_details(out: &mut String, report: &RunReport) {
    let mut keys = BTreeSet::new();
    for finding in &report.contenders[0].assurance {
        keys.insert((finding.category.clone(), finding.criterion.clone()));
    }
    for (category, criterion) in keys {
        writeln!(out, "### {}: {}\n", title_case(&category), criterion).unwrap();
        for contender in &report.contenders {
            if let Some(finding) = contender
                .assurance
                .iter()
                .find(|item| item.category == category && item.criterion == criterion)
            {
                writeln!(
                    out,
                    "- **{} - {}:** {}",
                    contender.display_name,
                    status_label(finding.status),
                    finding.summary
                )
                .unwrap();
                for evidence in &finding.evidence {
                    writeln!(out, "  - Evidence: `{}`", evidence).unwrap();
                }
            }
        }
        writeln!(out).unwrap();
    }
}

fn performance_scores(report: &RunReport) -> BTreeMap<String, f64> {
    let mut logs: BTreeMap<String, Vec<f64>> = report
        .contenders
        .iter()
        .map(|contender| (contender.id.clone(), Vec::new()))
        .collect();
    for name in CORE_METRICS {
        let values: Vec<_> = report
            .contenders
            .iter()
            .filter_map(|contender| {
                metric(contender, name)
                    .and_then(median)
                    .map(|v| (&contender.id, v))
            })
            .collect();
        if values.len() != report.contenders.len() {
            return report
                .contenders
                .iter()
                .map(|contender| (contender.id.clone(), 0.0))
                .collect();
        }
        let exemplar = report
            .contenders
            .iter()
            .find_map(|contender| metric(contender, name));
        let direction = exemplar
            .map(|metric| metric.direction)
            .unwrap_or(MetricDirection::Neutral);
        if direction == MetricDirection::Neutral {
            continue;
        }
        let floor = index_floor(exemplar.map(|metric| metric.unit.as_str()).unwrap_or(""));
        let raw: Vec<f64> = values.iter().map(|(_, value)| *value).collect();
        for (id, value) in values {
            let ratio = index_ratio(&raw, value, direction, floor);
            logs.get_mut(id).unwrap().push(ratio.ln());
        }
    }
    logs.into_iter()
        .map(|(id, values)| {
            let score = if values.is_empty() {
                0.0
            } else {
                (values.iter().sum::<f64>() / values.len() as f64).exp() * 100.0
            };
            (id, score)
        })
        .collect()
}

/// Absolute noise floor applied before a metric enters the balanced index. Idle CPU near zero is
/// dominated by scheduler tick resolution and background noise; without a floor, 0.00 versus 0.03
/// percent of a core would become a 100x ratio and dominate the geometric mean.
pub fn index_floor(unit: &str) -> f64 {
    match unit {
        "% core" => 0.1,
        _ => 0.0,
    }
}

/// Ratio of the best value to this value (1.0 is best). Values within one percent of the best are
/// treated as ties, matching the per-metric tables; ratios are bounded at 0.01.
pub fn index_ratio(values: &[f64], value: f64, direction: MetricDirection, floor: f64) -> f64 {
    let value = value.max(floor);
    let adjusted: Vec<f64> = values.iter().map(|v| v.max(floor)).collect();
    let (best, ratio) = match direction {
        MetricDirection::Lower => {
            let best = adjusted.iter().copied().fold(f64::INFINITY, f64::min);
            (best, if value <= 0.0 { 1.0 } else { best / value })
        }
        MetricDirection::Higher => {
            let best = adjusted.iter().copied().fold(0.0, f64::max);
            (best, if best <= 0.0 { 1.0 } else { value / best })
        }
        MetricDirection::Neutral => return 1.0,
    };
    let tolerance = best.abs().max(0.001) * 0.01;
    if (value - best).abs() <= tolerance {
        return 1.0;
    }
    ratio.clamp(0.01, 1.0)
}

fn assurance_scores(contenders: &[ContenderResult]) -> BTreeMap<String, BTreeMap<String, f64>> {
    let mut totals: BTreeMap<String, BTreeMap<String, (f64, f64)>> = BTreeMap::new();
    for contender in contenders {
        for finding in &contender.assurance {
            let Some(score) = finding.status.score() else {
                continue;
            };
            let entry = totals
                .entry(finding.category.clone())
                .or_default()
                .entry(contender.id.clone())
                .or_default();
            entry.0 += score * finding.weight;
            entry.1 += finding.weight;
        }
    }
    totals
        .into_iter()
        .map(|(category, values)| {
            let scores = values
                .into_iter()
                .filter_map(|(id, (total, weight))| (weight > 0.0).then_some((id, total / weight)))
                .collect();
            (category, scores)
        })
        .collect()
}

fn ordered_metric_names(report: &RunReport) -> Vec<String> {
    let mut names = BTreeSet::new();
    for contender in &report.contenders {
        for benchmark in &contender.benchmarks {
            names.insert(benchmark.name.clone());
        }
    }
    let mut result = Vec::new();
    for core in CORE_METRICS {
        if names.remove(*core) {
            result.push((*core).to_owned());
        }
    }
    result.extend(names);
    result
}

fn metric<'a>(contender: &'a ContenderResult, name: &str) -> Option<&'a BenchmarkResult> {
    raw_metric(contender, name).filter(|benchmark| benchmark.status == BenchmarkStatus::Measured)
}

fn raw_metric<'a>(contender: &'a ContenderResult, name: &str) -> Option<&'a BenchmarkResult> {
    contender
        .benchmarks
        .iter()
        .find(|benchmark| benchmark.name == name)
}

fn median(metric: &BenchmarkResult) -> Option<f64> {
    metric.summary.median
}

fn winner_index(values: &[Option<f64>], direction: MetricDirection) -> Option<usize> {
    if direction == MetricDirection::Neutral || values.iter().any(Option::is_none) {
        return None;
    }
    values
        .iter()
        .enumerate()
        .filter_map(|(index, value)| value.map(|value| (index, value)))
        .min_by(|left, right| match direction {
            MetricDirection::Lower => left.1.total_cmp(&right.1),
            MetricDirection::Higher => right.1.total_cmp(&left.1),
            MetricDirection::Neutral => std::cmp::Ordering::Equal,
        })
        .map(|(index, _)| index)
}

fn winner_label(report: &RunReport, values: &[Option<f64>], direction: MetricDirection) -> String {
    if direction == MetricDirection::Neutral || values.iter().any(Option::is_none) {
        return "not ranked".into();
    }
    let Some(best_index) = winner_index(values, direction) else {
        return "not ranked".into();
    };
    let best = values[best_index].unwrap_or_default();
    let tolerance = best.abs().max(0.001) * 0.01;
    let tied = values
        .iter()
        .flatten()
        .filter(|value| (**value - best).abs() <= tolerance)
        .count();
    if tied > 1 {
        "tie (within 1%)".into()
    } else {
        report.contenders[best_index].display_name.clone()
    }
}

fn format_value(value: f64, unit: &str) -> String {
    match unit {
        "bytes" if value >= 1024.0 * 1024.0 => format!("{:.2} MiB", value / 1024.0 / 1024.0),
        "bytes" if value >= 1024.0 => format!("{:.1} KiB", value / 1024.0),
        "KiB" if value >= 1024.0 => format!("{:.2} MiB", value / 1024.0),
        "ms" | "ms CPU" => format!("{value:.2}"),
        "%" => format!("{value:.1}"),
        "KiB/pane" => format!("{value:.0}"),
        "% core" | "MiB/s" => format!("{value:.3}"),
        _ if value.fract().abs() < 0.000_001 => format!("{value:.0}"),
        _ => format!("{value:.2}"),
    }
}

fn static_row(
    out: &mut String,
    report: &RunReport,
    label: &str,
    value: impl Fn(&ContenderResult) -> Option<u64>,
) {
    writeln!(
        out,
        "| {} | {} |",
        label,
        report
            .contenders
            .iter()
            .map(|contender| value(contender)
                .map(|value| value.to_string())
                .unwrap_or_else(|| "N/A".into()))
            .collect::<Vec<_>>()
            .join(" | ")
    )
    .unwrap();
}

fn contender_headers(report: &RunReport) -> String {
    report
        .contenders
        .iter()
        .map(|item| item.display_name.clone())
        .collect::<Vec<_>>()
        .join(" | ")
}

fn host_label(report: &RunReport) -> String {
    format!(
        "{}{} / {} ({})",
        if report.host.wsl { "WSL " } else { "" },
        report.host.os,
        report.host.architecture,
        report.host.hostname
    )
}

fn contender<'a>(report: &'a RunReport, id: &str) -> Option<&'a ContenderResult> {
    report.contenders.iter().find(|item| item.id == id)
}

fn display_name(report: &RunReport, id: &str) -> String {
    contender(report, id)
        .map(|item| item.display_name.clone())
        .unwrap_or_else(|| id.to_owned())
}

fn title_case(value: &str) -> String {
    let mut chars = value.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

fn status_label(status: AssuranceStatus) -> &'static str {
    match status {
        AssuranceStatus::Pass => "pass",
        AssuranceStatus::Partial => "partial",
        AssuranceStatus::Fail => "fail",
        AssuranceStatus::Unknown => "unknown",
        AssuranceStatus::NotApplicable => "not applicable",
    }
}

fn escape_cell(value: &str) -> String {
    value.replace('|', "\\|").replace('\n', " ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn three_contenders() -> RunReport {
        use crate::model::*;
        RunReport {
            schema_version: RESULT_SCHEMA_VERSION,
            tool_version: "test".into(),
            run_id: "test".into(),
            started_unix_ms: 0,
            profile: crate::config::profile("smoke").unwrap(),
            warnings: vec![],
            host: HostInfo {
                os: "linux".into(),
                architecture: "x86_64".into(),
                kernel: "test".into(),
                hostname: "test".into(),
                logical_cpus: 1,
                rustc: None,
                wsl: false,
                git_dirty_policy: "test".into(),
                cpu_time_source: "test".into(),
            },
            fairness: FairnessRecord {
                run_order: vec![],
                release_binaries_required: true,
                network_disabled_during_benchmarks: true,
                isolated_home_and_xdg: true,
                identical_terminal_geometry: true,
                child_workload: "test".into(),
                notes: vec![],
            },
            contenders: ["uniterm", "herdr", "tmux"]
                .into_iter()
                .map(|id| ContenderResult {
                    id: id.into(),
                    display_name: id.into(),
                    adapter: id.into(),
                    binary: ArtifactInfo {
                        path: "test".into(),
                        bytes: 1,
                        version_output: "test".into(),
                        sha256: "a".repeat(64),
                    },
                    source: SourceInfo {
                        path: "test".into(),
                        commit: Some("b".repeat(40)),
                        commit_date: None,
                        dirty: Some(false),
                        package_version: None,
                        license: None,
                    },
                    static_analysis: StaticAnalysis::default(),
                    assurance: vec![],
                    errors: vec![],
                    benchmarks: CORE_METRICS
                        .iter()
                        .map(|name| {
                            measured_benchmark(
                                *name,
                                "ms",
                                MetricDirection::Lower,
                                vec![1.0],
                                "fixture",
                            )
                        })
                        .collect(),
                })
                .collect(),
        }
    }

    #[test]
    fn three_way_ranking_requires_every_contenders_core_metrics() {
        let mut report = three_contenders();
        assert!(performance_scores(&report)
            .values()
            .all(|score| *score == 100.0));
        report.contenders[2].benchmarks[0] =
            crate::model::failed_benchmark(CORE_METRICS[0], "probe failed");
        assert!(performance_scores(&report)
            .values()
            .all(|score| *score == 0.0));
        let markdown = markdown("test", &[report]).unwrap();
        assert!(markdown.contains("FAILED"));
        assert!(!markdown.contains("ranks first"));
    }

    #[test]
    fn restoration_na_is_context_and_never_hides_failure() {
        let mut report = three_contenders();
        let mut restart = crate::model::measured_benchmark(
            "restart_ready",
            "ms",
            MetricDirection::Neutral,
            vec![1.0],
            "fresh session",
        );
        restart
            .metadata
            .insert("native_disk_restoration".into(), "not_applicable".into());
        report.contenders[2].benchmarks.push(restart);
        assert_eq!(
            restoration_cell(&report.contenders[2]),
            "N/A (no native disk restoration)"
        );
        assert!(performance_scores(&report)
            .values()
            .all(|score| *score == 100.0));
        report.contenders[2].benchmarks.last_mut().unwrap().status = BenchmarkStatus::Failed;
        assert_eq!(restoration_cell(&report.contenders[2]), "FAILED");
        assert_eq!(metric_cell(None, None, "ms"), "missing");
    }

    #[test]
    fn merge_rejects_schema_adapter_source_and_contender_changes() {
        let baseline = three_contenders();
        for mutate in [
            (|r: &mut RunReport| r.schema_version = 5) as fn(&mut RunReport),
            |r| r.contenders[2].adapter = "uniterm".into(),
            |r| r.contenders[2].source.commit = Some("def".into()),
            |r| {
                r.contenders.pop();
            },
        ] {
            let mut changed = baseline.clone();
            mutate(&mut changed);
            assert!(validate_merge(&[baseline.clone(), changed]).is_err());
        }
        assert!(validate_merge(&[baseline.clone(), baseline]).is_ok());
    }

    #[test]
    fn schema_five_integer_static_counts_remain_readable() {
        let mut value = serde_json::to_value(three_contenders()).unwrap();
        value["schema_version"] = 5.into();
        value["contenders"][0]["static_analysis"]["unsafe_blocks"] = 0.into();
        let report: RunReport = serde_json::from_value(value).unwrap();
        assert_eq!(report.contenders[0].static_analysis.unsafe_blocks, Some(0));
        assert!(markdown("archive", &[report]).is_ok());
    }

    #[test]
    fn lower_and_higher_winners_are_selected() {
        let values = vec![Some(2.0), Some(1.0)];
        assert_eq!(winner_index(&values, MetricDirection::Lower), Some(1));
        assert_eq!(winner_index(&values, MetricDirection::Higher), Some(0));
        assert_eq!(winner_index(&values, MetricDirection::Neutral), None);
    }

    #[test]
    fn index_ratio_treats_near_zero_cpu_as_a_tie() {
        let values = [0.0, 0.03];
        assert_eq!(index_ratio(&values, 0.0, MetricDirection::Lower, 0.1), 1.0);
        assert_eq!(index_ratio(&values, 0.03, MetricDirection::Lower, 0.1), 1.0);
        let values = [0.0, 0.5];
        assert!((index_ratio(&values, 0.5, MetricDirection::Lower, 0.1) - 0.2).abs() < 1e-9);
    }

    #[test]
    fn index_ratio_uses_one_percent_ties_and_lower_bound() {
        let values = [100.0, 100.9, 200.0];
        assert_eq!(
            index_ratio(&values, 100.9, MetricDirection::Lower, 0.0),
            1.0
        );
        assert!((index_ratio(&values, 200.0, MetricDirection::Lower, 0.0) - 0.5).abs() < 1e-9);
        assert_eq!(
            index_ratio(&[1.0, 1000.0], 1000.0, MetricDirection::Lower, 0.0),
            0.01
        );
        assert!((index_ratio(&[8.0, 4.0], 4.0, MetricDirection::Higher, 0.0) - 0.5).abs() < 1e-9);
    }

    #[test]
    fn missing_value_prevents_a_claimed_winner() {
        assert_eq!(
            winner_index(&[Some(1.0), None], MetricDirection::Lower),
            None
        );
    }
}
