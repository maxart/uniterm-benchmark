use std::collections::BTreeMap;
use std::process::Command;
use ut_compare::adapters::IsolatedWorkdir;
use ut_compare::model::*;
use ut_compare::{privacy, report};

fn private_text() -> String {
    format!(
        "private-sentinel {}{} /home/test-person/secret folder alice@example.invalid",
        "ghp_",
        "A".repeat(36)
    )
}

fn fixture() -> RunReport {
    let private = private_text();
    let contender = |id: &str, adapter: &str| ContenderResult {
        id: id.into(),
        display_name: private.clone(),
        adapter: adapter.into(),
        binary: ArtifactInfo {
            path: private.clone(),
            bytes: 123,
            version_output: format!("{adapter} 1.2.3 {private}"),
            sha256: "a".repeat(64),
        },
        source: SourceInfo {
            path: private.clone(),
            commit: Some("b".repeat(40)),
            commit_date: Some(private.clone()),
            dirty: Some(false),
            package_version: Some("1.2.3".into()),
            license: Some(private.clone()),
        },
        static_analysis: StaticAnalysis {
            notes: vec![private.clone()],
            ..Default::default()
        },
        assurance: vec![AssuranceFinding {
            category: "security".into(),
            criterion: "Memory-safety boundary".into(),
            status: AssuranceStatus::Partial,
            weight: 1.0,
            summary: private.clone(),
            evidence: vec![private.clone()],
        }],
        errors: vec![],
        benchmarks: [
            "server_startup_ready",
            "control_command_latency",
            "daemon_idle_cohort_cpu",
            "daemon_idle_cohort_rss",
            "foreground_idle_cohort_cpu",
            "foreground_idle_cohort_rss",
            "terminal_input_to_visible",
            "terminal_output_completion",
        ]
        .into_iter()
        .map(|name| {
            let mut b = measured_benchmark(
                name,
                "ms",
                MetricDirection::Lower,
                vec![1.25, 2.5, 3.75],
                &private,
            );
            b.metadata = BTreeMap::from([
                ("private-data".into(), private.clone()),
                ("pane_size_rows_cols".into(), "40 120".into()),
                ("exit_retries".into(), private.clone()),
            ]);
            b
        })
        .collect(),
    };
    RunReport {
        schema_version: 6,
        tool_version: "0.2.0".into(),
        run_id: private.clone(),
        started_unix_ms: 123456,
        host: HostInfo {
            os: "linux".into(),
            architecture: "x86_64".into(),
            kernel: format!("Linux 6.8.0-{private}"),
            hostname: private.clone(),
            logical_cpus: 4,
            rustc: Some(format!("rustc 1.85.0 {private}")),
            wsl: false,
            git_dirty_policy: private.clone(),
            cpu_time_source: format!("/proc/<pid>/stat utime+stime (10 ms ticks); {private}"),
        },
        profile: ut_compare::config::profile("smoke").unwrap(),
        fairness: FairnessRecord {
            run_order: vec!["private-first".into(), "private-second".into()],
            release_binaries_required: true,
            network_disabled_during_benchmarks: true,
            isolated_home_and_xdg: true,
            identical_terminal_geometry: true,
            child_workload: private.clone(),
            notes: vec![private.clone()],
        },
        contenders: vec![
            contender("private-first", "uniterm"),
            contender("private-second", "herdr"),
        ],
        warnings: vec![private],
    }
}

#[test]
fn removes_identifiers_and_untrusted_text_without_changing_measurements() {
    let original = fixture();
    let safe = privacy::sanitize(&original).unwrap();
    let json = serde_json::to_string(&safe).unwrap();
    let markdown = report::markdown(&private_text(), std::slice::from_ref(&original)).unwrap();
    for output in [&json, &markdown] {
        for secret in [
            "private-sentinel",
            "private-first",
            "private-second",
            "/home/test-person",
            "alice@",
            "secret folder",
            "ghp_",
        ] {
            assert!(
                !output.contains(secret),
                "output leaked a forbidden fixture field"
            );
        }
    }
    assert!(!json.contains("started_unix_ms"));
    assert_eq!(safe.schema_version, RESULT_SCHEMA_VERSION);
    assert_eq!(
        safe.host.cpu_time_source,
        "/proc/<pid>/stat (10 ms ticks); ps supplies RSS and process ancestry"
    );
    for (before, after) in original.contenders.iter().zip(&safe.contenders) {
        assert_eq!(before.binary.sha256, after.binary.sha256);
        assert_eq!(before.source.commit, after.source.commit);
        assert_eq!(before.assurance[0].status, after.assurance[0].status);
        for (before, after) in before.benchmarks.iter().zip(&after.benchmarks) {
            assert_eq!(before.samples, after.samples);
            assert_eq!(
                serde_json::to_value(&before.summary).unwrap(),
                serde_json::to_value(&after.summary).unwrap()
            );
            assert_eq!(before.status, after.status);
            assert_eq!(after.metadata.len(), 1);
        }
    }
    assert_eq!(
        serde_json::to_value(&safe).unwrap(),
        serde_json::to_value(privacy::sanitize(&safe).unwrap()).unwrap()
    );
}

#[test]
fn workdir_tree_is_owner_only_and_unicode_labels_are_safe() {
    use std::os::unix::fs::PermissionsExt;
    let tree = IsolatedWorkdir::create("🔒private", false).unwrap();
    let root = tree.root.clone();
    for path in [
        &tree.root,
        &tree.home,
        &tree.runtime,
        &tree.state,
        &tree.config,
    ] {
        assert_eq!(
            std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }
    drop(tree);
    assert!(!root.exists());
}

#[test]
fn concurrent_workdirs_have_distinct_names_and_independent_cleanup() {
    use std::collections::BTreeSet;
    use std::sync::{Arc, Barrier};
    let barrier = Arc::new(Barrier::new(32));
    let threads: Vec<_> = (0..32)
        .map(|_| {
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                barrier.wait();
                IsolatedWorkdir::create("parallel", false).unwrap()
            })
        })
        .collect();
    let workdirs: Vec<_> = threads.into_iter().map(|t| t.join().unwrap()).collect();
    let roots: BTreeSet<_> = workdirs.iter().map(|w| w.root.clone()).collect();
    assert_eq!(roots.len(), 32);
    assert!(roots.iter().all(|root| root.is_dir()));
    drop(workdirs);
    assert!(roots.iter().all(|root| !root.exists()));
}

#[test]
fn aliases_are_idempotent_for_double_digit_contender_counts() {
    let mut original = fixture();
    let template = original.contenders[0].clone();
    original.contenders = (0..12)
        .map(|n| {
            let mut contender = template.clone();
            contender.id = format!("private-instance-{n}");
            contender
        })
        .collect();
    original.fairness.run_order = original.contenders.iter().map(|c| c.id.clone()).collect();
    let safe = privacy::sanitize(&original).unwrap();
    assert_eq!(
        serde_json::to_value(&safe).unwrap(),
        serde_json::to_value(privacy::sanitize(&safe).unwrap()).unwrap()
    );
}

#[test]
fn private_version_suffixes_are_withheld_without_claiming_a_stable_release() {
    assert_eq!(privacy::version("1.2.3"), Some("1.2.3".into()));
    assert_eq!(privacy::version("3.7c"), Some("3.7c".into()));
    let safe = privacy::version("1.2.3-private-sentinel").unwrap();
    assert_eq!(safe, "1.2.3-redacted");
    assert_eq!(privacy::version(&safe), Some(safe));
}

#[test]
fn failed_metrics_stay_failed_and_suppress_ranking() {
    let mut original = fixture();
    original.contenders[0].benchmarks[0] = failed_benchmark("server_startup_ready", private_text());
    original.contenders[0].errors.push(private_text());
    let safe = privacy::sanitize(&original).unwrap();
    assert!(report::has_failures(&safe));
    assert_eq!(
        safe.contenders[0].benchmarks[0].status,
        BenchmarkStatus::Failed
    );
    let markdown = report::markdown("ignored", &[safe]).unwrap();
    assert!(markdown.contains("FAILED"));
    assert!(!markdown.contains("ranks first"));
    assert!(!markdown.contains("private-sentinel"));
}

#[test]
fn unknown_metric_and_malicious_hash_fail_closed() {
    let mut original = fixture();
    original.contenders[0].benchmarks[0].name = private_text();
    assert!(privacy::sanitize(&original).is_err());
    let mut original = fixture();
    original.contenders[0].binary.sha256 = private_text();
    assert!(privacy::sanitize(&original).is_err());
}

#[test]
fn cli_filters_legacy_json_markdown_titles_and_diagnostics() {
    let tree = IsolatedWorkdir::create("privacy", false).unwrap();
    let input = tree.root.join("private-sentinel-input.json");
    let json = tree.root.join("private-sentinel-output.json");
    let md = tree.root.join("private-sentinel-output.md");
    let mut original = fixture();
    // Synthetic adjacent IEEE-754 values expose parsers that round off the final bit.
    let samples: Vec<_> = [0.1_f64, 1.0, 10.0, 100.0]
        .into_iter()
        .flat_map(|base| (0..100).map(move |n| f64::from_bits(base.to_bits() + n)))
        .collect();
    original.contenders[0].benchmarks[0] = measured_benchmark(
        "server_startup_ready",
        "ms",
        MetricDirection::Lower,
        samples,
        "synthetic values",
    );
    std::fs::write(&input, serde_json::to_vec(&original).unwrap()).unwrap();
    let exe = env!("CARGO_BIN_EXE_ut-compare");
    let output = Command::new(exe)
        .arg("sanitize")
        .arg(&input)
        .arg("--output")
        .arg(&json)
        .arg("--markdown")
        .arg(&md)
        .output()
        .unwrap();
    assert!(output.status.success());
    let converted: RunReport = serde_json::from_slice(&std::fs::read(&json).unwrap()).unwrap();
    assert_eq!(
        original.contenders[0].benchmarks[0].samples,
        converted.contenders[0].benchmarks[0].samples
    );
    assert_eq!(
        serde_json::to_value(&original.contenders[0].benchmarks[0].summary).unwrap(),
        serde_json::to_value(&converted.contenders[0].benchmarks[0].summary).unwrap()
    );
    for text in [
        String::from_utf8(output.stdout).unwrap(),
        String::from_utf8(output.stderr).unwrap(),
        std::fs::read_to_string(&json).unwrap(),
        std::fs::read_to_string(&md).unwrap(),
    ] {
        assert!(!text.contains("private-sentinel"));
        assert!(!text.contains("ghp_"));
        assert!(!text.contains(tree.root.to_str().unwrap()));
    }
    let rendered = std::fs::read_to_string(&md).unwrap();
    let output = Command::new(exe)
        .arg("report")
        .arg(&input)
        .arg("--title")
        .arg(private_text())
        .arg("--output")
        .arg(&md)
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(rendered, std::fs::read_to_string(&md).unwrap());
    std::fs::write(&input, format!("not-json {}", private_text())).unwrap();
    let output = Command::new(exe)
        .arg("sanitize")
        .arg(&input)
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(!stderr.contains("private-sentinel"));
    assert!(!stderr.contains(tree.root.to_str().unwrap()));
    assert!(stderr.contains("omitted for privacy"));
}

#[test]
fn cli_doctor_does_not_echo_product_output_or_config_parse_errors() {
    use std::os::unix::fs::PermissionsExt;
    let tree = IsolatedWorkdir::create("privacy", false).unwrap();
    let binary = tree.root.join("private-sentinel-binary");
    std::fs::write(
        &binary,
        "#!/bin/sh\nprintf '%s\\n' 'uniterm 1.2.3 private-sentinel /home/test-person'\n",
    )
    .unwrap();
    std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o700)).unwrap();
    let config = tree.root.join("private-sentinel.toml");
    let contender = |id| {
        format!("[[contenders]]\nid='{id}'\ndisplay_name='private-sentinel'\nadapter='uniterm'\nbinary='{}'\nsource='{}'\n", binary.display(), tree.root.display())
    };
    std::fs::write(
        &config,
        format!(
            "[comparison]\ntitle='private-sentinel'\n{}\n{}",
            contender("first"),
            contender("second")
        ),
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_ut-compare"))
        .arg("doctor")
        .arg("--config")
        .arg(&config)
        .output()
        .unwrap();
    assert!(output.status.success());
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!text.contains("private-sentinel"));
    assert!(!text.contains(tree.root.to_str().unwrap()));
    std::fs::write(&config, format!("invalid TOML {}", private_text())).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_ut-compare"))
        .arg("doctor")
        .arg("--config")
        .arg(&config)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(!String::from_utf8_lossy(&output.stderr).contains("private-sentinel"));
}

#[test]
fn imported_schemas_are_compared_before_redaction() {
    let tree = IsolatedWorkdir::create("privacy", false).unwrap();
    let first = tree.root.join("first.json");
    let second = tree.root.join("second.json");
    let a = fixture();
    let mut b = a.clone();
    b.schema_version = 5;
    for (path, value) in [(&first, &a), (&second, &b)] {
        std::fs::write(path, serde_json::to_vec(value).unwrap()).unwrap();
    }
    assert!(report::load_reports(&[first, second]).is_err());
}
