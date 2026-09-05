use crate::adapters::{
    command_latency, path_is_release_binary, wait_for_process_count,
    wait_for_process_count_at_most, AppAdapter, Session,
};
use crate::audit;
use crate::config::Config;
use crate::model::{
    failed_benchmark, measured_benchmark, ContenderResult, FairnessRecord, MetricDirection,
    ProfileRecord, RunReport, RESULT_SCHEMA_VERSION,
};
use crate::process::{
    cpu_percent, host_info, sample_cohort, sample_cohorts, CohortMetrics, ProcessSnapshot,
};
use crate::pty::PtyChild;
use crate::screen::Screen;
use std::collections::BTreeMap;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub fn run(config: &Config, profile: ProfileRecord) -> Result<RunReport, String> {
    if config.comparison.keep_workdirs {
        return Err("raw workdirs cannot be retained".into());
    }
    let result =
        run_internal(config, profile).map_err(|error| crate::privacy::public_error(&error))?;
    crate::privacy::sanitize(&result)
}

fn run_internal(config: &Config, profile: ProfileRecord) -> Result<RunReport, String> {
    let started_unix_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let run_id = format!("utc-{started_unix_ms}-{:016x}", config.comparison.seed);
    let mut warnings = Vec::new();
    let mut contenders = Vec::new();
    for contender in &config.contenders {
        let binary = audit::artifact(contender)?;
        if !path_is_release_binary(&contender.binary) {
            warnings.push(format!(
                "{} binary path does not contain a release component: {}",
                contender.id,
                contender.binary.display()
            ));
        }
        contenders.push(ContenderResult {
            id: contender.id.clone(),
            display_name: contender.display_name.clone(),
            adapter: contender.adapter.clone(),
            binary,
            source: audit::source_info(contender),
            static_analysis: audit::static_analysis(&contender.source),
            assurance: contender.assurance.iter().map(Into::into).collect(),
            benchmarks: Vec::new(),
            errors: Vec::new(),
        });
    }

    let run_order = rotated_order(config.contenders.len(), config.comparison.seed);
    for &index in &run_order {
        let bytes = contenders[index].binary.bytes as f64;
        contenders[index].benchmarks.push(measured_benchmark(
            "binary_size",
            "bytes",
            MetricDirection::Lower,
            vec![bytes],
            "Exact executable file size; symbols and packaging depend on the supplied build.",
        ));
    }

    run_startup_trials(config, &profile, &run_order, &mut contenders);
    for &index in &run_order {
        run_live_suite(
            config,
            &profile,
            index,
            &mut contenders[index],
            10_000 + index,
        );
    }

    Ok(RunReport {
        schema_version: RESULT_SCHEMA_VERSION,
        tool_version: env!("CARGO_PKG_VERSION").into(),
        run_id,
        started_unix_ms,
        host: host_info(),
        profile: profile.clone(),
        fairness: FairnessRecord {
            run_order: run_order
                .iter()
                .map(|index| config.contenders[*index].id.clone())
                .collect(),
            release_binaries_required: true,
            network_disabled_during_benchmarks: true,
            isolated_home_and_xdg: true,
            identical_terminal_geometry: true,
            child_workload: format!(
                "POSIX /bin/sh, {}x{}, {} deterministic 72-byte lines",
                profile.terminal_cols, profile.terminal_rows, profile.output_lines
            ),
            notes: vec![
                "Terminal latency and output completion use a real PTY for all contenders.".into(),
                "Control APIs are used only for readiness, status probes, and teardown.".into(),
                "Each app receives a private HOME and XDG tree; Herdr update and manifest checks are disabled during timing.".into(),
                "Startup is timed until an identical pane-listing probe succeeds; detached idle is sampled after one attach/detach so all servers hold exactly one pane shell.".into(),
                "Herdr's headless render grid is pinned to the attached geometry.".into(),
                "When tmux is included, every command uses a private -S socket and explicit -f config; /bin/sh is non-login, TERM matches the workload, and prefix-key splits tile the window. Status, history, and rendering timers retain defaults.".into(),
                "Every latency and output trial is verified against a modelled screen; a wrong or incomplete final screen fails the metric instead of scoring as fast.".into(),
                "Resize-storm (over the output-workload scrollback), multi-client (fresh session), pane-scaling/recovery (fresh session), and restart scenarios are reported as context; they are not folded into the balanced index.".into(),
                "CPU percent is one-core percentage from cumulative process CPU time, not percentage of the whole machine.".into(),
                "Root metrics cover server and attached client roots; cohort metrics also include their descendants.".into(),
            ],
        },
        contenders,
        warnings,
    })
}

fn run_startup_trials(
    config: &Config,
    profile: &ProfileRecord,
    base_order: &[usize],
    results: &mut [ContenderResult],
) {
    let mut startup_samples = vec![Vec::new(); config.contenders.len()];
    let mut shutdown_samples = vec![Vec::new(); config.contenders.len()];
    for trial in 0..profile.startup_iterations {
        let order: Vec<usize> = if trial % 2 == 0 {
            base_order.to_vec()
        } else {
            base_order.iter().rev().copied().collect()
        };
        for index in order {
            let adapter = AppAdapter::new(
                config.contenders[index].clone(),
                profile.terminal_cols,
                profile.terminal_rows,
            );
            match adapter.start(trial * 100 + index, config.comparison.keep_workdirs) {
                Ok(started) => {
                    startup_samples[index].push(started.startup_elapsed.as_secs_f64() * 1_000.0);
                    let mut session = started.session;
                    match session.stop() {
                        Ok(duration) => {
                            shutdown_samples[index].push(duration.as_secs_f64() * 1_000.0)
                        }
                        Err(error) => results[index]
                            .errors
                            .push(format!("startup trial teardown: {error}")),
                    }
                }
                Err(error) => results[index]
                    .errors
                    .push(format!("startup trial {}: {error}", trial + 1)),
            }
        }
    }
    for index in 0..results.len() {
        if startup_samples[index].len() != profile.startup_iterations {
            results[index].benchmarks.push(failed_benchmark(
                "server_startup_ready",
                "startup trial set is incomplete",
            ));
        } else {
            results[index].benchmarks.push(measured_benchmark(
                "server_startup_ready",
                "ms",
                MetricDirection::Lower,
                std::mem::take(&mut startup_samples[index]),
                "Product start command through the first successful pane-listing probe over the product socket; PID discovery is outside the timed window. Trial order reverses each round.",
            ));
        }
        if !shutdown_samples[index].is_empty() {
            results[index].benchmarks.push(measured_benchmark(
                "server_shutdown",
                "ms",
                MetricDirection::Lower,
                std::mem::take(&mut shutdown_samples[index]),
                "Graceful product command through observed server exit and persistence flush.",
            ));
        }
    }
}

fn run_live_suite(
    config: &Config,
    profile: &ProfileRecord,
    index: usize,
    result: &mut ContenderResult,
    sequence: usize,
) {
    let adapter = AppAdapter::new(
        config.contenders[index].clone(),
        profile.terminal_cols,
        profile.terminal_rows,
    );
    let started = match adapter.start(sequence, config.comparison.keep_workdirs) {
        Ok(started) => started,
        Err(error) => {
            result.errors.push(format!("live suite startup: {error}"));
            return;
        }
    };
    let mut session = started.session;

    // Put both products into the same detached state before sampling: one attach, one pane
    // shell running, then detach. Uniterm starts its first pane shell with the Workspace while
    // Herdr's headless server has no pane until a client attaches; sampling straight after
    // startup would compare a server with a live shell against a server with none.
    if let Err(error) = prime_detached_session(&session) {
        result.errors.push(format!("daemon idle priming: {error}"));
        let _ = session.stop();
        return;
    }

    let idle_duration = Duration::from_secs_f64(profile.idle_seconds);
    let interval = Duration::from_millis(profile.sample_interval_ms);
    std::thread::sleep(Duration::from_secs_f64(profile.settle_seconds));
    match sample_cohort(session.root_pid, idle_duration, interval) {
        Ok((samples, elapsed)) => add_resource_metrics(
            result,
            "daemon_idle",
            &samples,
            elapsed,
            "Detached server plus its one pane shell after a single attach/detach; no app client is attached.",
        ),
        Err(error) => result
            .benchmarks
            .push(failed_benchmark("daemon_idle", error)),
    }

    match command_latency(|| session.status_command(), profile.command_iterations) {
        Ok(samples) => result.benchmarks.push(measured_benchmark(
            "control_command_latency",
            "ms",
            MetricDirection::Lower,
            samples,
            "Fresh CLI process listing the session's panes through the product socket.",
        )),
        Err(error) => result
            .benchmarks
            .push(failed_benchmark("control_command_latency", error)),
    }

    let mut client = match session.attach() {
        Ok(client) => client,
        Err(error) => {
            result.errors.push(format!("attach: {error}"));
            let _ = session.stop();
            return;
        }
    };
    if let Err(error) = client.drain_for(Duration::from_millis(500)) {
        result.errors.push(format!("initial client drain: {error}"));
    }

    run_terminal_latency(profile, result, &mut client);

    std::thread::sleep(Duration::from_secs_f64(profile.settle_seconds));
    match sample_cohorts(&[session.root_pid, client.pid()], idle_duration, interval) {
        Ok((samples, elapsed)) => add_resource_metrics(
            result,
            "foreground_idle",
            &samples,
            elapsed,
            "Server, attached client, pane shells, and their descendants at a stable foreground screen.",
        ),
        Err(error) => result
            .benchmarks
            .push(failed_benchmark("foreground_idle", error)),
    }

    run_output_workload(profile, result, &mut client);
    run_resize_storm(profile, result, &session, &mut client);

    result.benchmarks.push(measured_benchmark(
        "isolated_state_size",
        "bytes",
        MetricDirection::Neutral,
        vec![session.state_bytes() as f64],
        "All files in the isolated HOME/XDG benchmark tree after workloads; a context metric because products persist different semantics.",
    ));

    let _ = client.send(session.detach_sequence());
    client.terminate();
    match session.stop() {
        Ok(duration) => result.benchmarks.push(measured_benchmark(
            "live_suite_shutdown",
            "ms",
            MetricDirection::Lower,
            vec![duration.as_secs_f64() * 1_000.0],
            "Shutdown after the one-pane latency, output, and resize workloads.",
        )),
        Err(error) => result.errors.push(format!("live suite teardown: {error}")),
    }

    run_restart(
        result,
        &mut session,
        &format!(
            "UTC_OUTPUT_WORKLOAD_END_{:02}",
            profile.output_iterations.saturating_sub(1)
        ),
    );

    run_fresh_multipane_suite(
        config.comparison.keep_workdirs,
        profile,
        result,
        &adapter,
        sequence + 1,
    );
    run_fresh_multiclient_suite(
        config.comparison.keep_workdirs,
        profile,
        result,
        &adapter,
        sequence + 2,
    );
}

/// Multi-client cost is measured in a fresh session so scrollback and allocator retention from
/// the output burst and resize storm cannot bias it, mirroring the multi-pane rule.
fn run_fresh_multiclient_suite(
    keep_workdir: bool,
    profile: &ProfileRecord,
    result: &mut ContenderResult,
    adapter: &AppAdapter,
    sequence: usize,
) {
    if profile.extra_clients == 0 {
        return;
    }
    let started = match adapter.start(sequence, keep_workdir) {
        Ok(started) => started,
        Err(error) => {
            result
                .errors
                .push(format!("multi-client suite startup: {error}"));
            return;
        }
    };
    let mut session = started.session;
    let mut client = match session.attach() {
        Ok(client) => client,
        Err(error) => {
            result
                .errors
                .push(format!("multi-client suite attach: {error}"));
            let _ = session.stop();
            return;
        }
    };
    let _ = client.drain_for(Duration::from_millis(500));
    if let Err(error) = wait_for_process_count(session.root_pid, 2, Duration::from_secs(5)) {
        result
            .errors
            .push(format!("multi-client suite first pane: {error}"));
        let _ = session.stop();
        return;
    }
    run_multiclient(profile, result, &session, &mut client);
    let _ = client.send(session.detach_sequence());
    client.terminate();
    if let Err(error) = session.stop() {
        result
            .errors
            .push(format!("multi-client suite teardown: {error}"));
    }
}

fn prime_detached_session(session: &Session) -> Result<(), String> {
    let mut client = session
        .attach()
        .map_err(|error| format!("attach: {error}"))?;
    client.drain_for(Duration::from_millis(500))?;
    wait_for_process_count(session.root_pid, 2, Duration::from_secs(5))
        .map_err(|error| format!("{error} (server plus one pane shell)"))?;
    client.send(session.detach_sequence())?;
    client.drain_for(Duration::from_millis(300))?;
    client.terminate();
    Ok(())
}

fn run_terminal_latency(
    profile: &ProfileRecord,
    result: &mut ContenderResult,
    client: &mut PtyChild,
) {
    match latency_samples(profile.latency_iterations, client) {
        Ok(samples) => result.benchmarks.push(measured_benchmark(
            "terminal_input_to_visible",
            "ms",
            MetricDirection::Lower,
            samples,
            "PTY input write through an identical shell printf (including viewport clear/home), server parse/render, client render, and visible marker bytes. Every trial is oracle-checked: the new marker must be on the modelled screen and the previous marker must be gone.",
        )),
        Err(error) => {
            result.errors.push(format!("terminal latency: {error}"));
            result
                .benchmarks
                .push(failed_benchmark("terminal_input_to_visible", error));
        }
    }
}

/// Runs `iterations` input-to-visible trials on `client`, verifying the visible screen after
/// each one. Any observation or oracle failure aborts the metric; it is never a slow sample.
fn latency_samples(iterations: usize, client: &mut PtyChild) -> Result<Vec<f64>, String> {
    let mut samples = Vec::with_capacity(iterations);
    for iteration in 0..iterations {
        let marker = latency_marker(iteration);
        client
            .clear_observation()
            .map_err(|error| format!("latency clear: {error}"))?;
        // Reset the visible viewport for every trial. Without this, later samples exercise
        // scrolling while early samples do not (and a rows/2-sized run can hit the bottom at a
        // contender-specific point). The clear/home bytes and marker are octal-encoded so the
        // shell's input echo cannot satisfy marker detection before rendered output arrives.
        let command = format!("printf '\\033[2J\\033[H{}\\n'", shell_octal(&marker));
        let started = Instant::now();
        client
            .send_line(&command)
            .map_err(|error| format!("latency input: {error}"))?;
        client
            .read_until_text(&marker, Duration::from_secs(3))
            .map_err(|error| format!("latency observation: {error}"))?;
        samples.push(started.elapsed().as_secs_f64() * 1_000.0);
        // Let the client finish the frame before judging the screen; this is outside the sample.
        client.drain_for(Duration::from_millis(30))?;
        let previous = (iteration > 0).then(|| latency_marker(iteration - 1));
        verify_latency_screen(client.screen(), &marker, previous.as_deref())
            .map_err(|error| format!("trial {}: {error}", iteration + 1))?;
    }
    if samples.len() != iterations {
        return Err(format!(
            "only {}/{} required markers were observed",
            samples.len(),
            iterations
        ));
    }
    Ok(samples)
}

fn verify_latency_screen(
    screen: &Screen,
    marker: &str,
    previous: Option<&str>,
) -> Result<(), String> {
    if screen.count(marker) == 0 {
        return Err(format!(
            "oracle: marker {marker} was emitted but is not on the visible screen\n{}",
            screen.dump()
        ));
    }
    if let Some(previous) = previous {
        if screen.count(previous) > 0 {
            return Err(format!(
                "oracle: stale marker {previous} is still visible after the viewport clear\n{}",
                screen.dump()
            ));
        }
    }
    Ok(())
}

fn run_fresh_multipane_suite(
    keep_workdir: bool,
    profile: &ProfileRecord,
    result: &mut ContenderResult,
    adapter: &AppAdapter,
    sequence: usize,
) {
    let started = match adapter.start(sequence, keep_workdir) {
        Ok(started) => started,
        Err(error) => {
            result
                .errors
                .push(format!("multi-pane suite startup: {error}"));
            return;
        }
    };
    let mut session = started.session;
    let mut client = match session.attach() {
        Ok(client) => client,
        Err(error) => {
            result
                .errors
                .push(format!("multi-pane suite attach: {error}"));
            let _ = session.stop();
            return;
        }
    };
    let _ = client.drain_for(Duration::from_millis(500));
    run_multipane(profile, result, &session, &mut client);
    let _ = client.send(session.detach_sequence());
    client.terminate();
    match session.stop() {
        Ok(duration) => result.benchmarks.push(measured_benchmark(
            "multipane_suite_shutdown",
            "ms",
            MetricDirection::Lower,
            vec![duration.as_secs_f64() * 1_000.0],
            "Shutdown of the fresh multi-pane session after scaling, resource sampling, and pane closing.",
        )),
        Err(error) => result
            .errors
            .push(format!("multi-pane suite teardown: {error}")),
    }
}

fn run_multipane(
    profile: &ProfileRecord,
    result: &mut ContenderResult,
    session: &Session,
    client: &mut PtyChild,
) {
    if profile.pane_count <= 1 {
        return;
    }
    let roots = [session.root_pid, client.pid()];
    let interval = Duration::from_millis(profile.sample_interval_ms);

    // Grow one pane at a time. Each step waits for the new pane shell to exist (a validity
    // condition, not a timing) and records the cohort RSS so a per-pane memory slope can be fit.
    let mut scaling: Vec<(usize, f64)> = Vec::new();
    for panes in 1..=profile.pane_count {
        if panes > 1 {
            if let Err(error) = client.send(session.split_sequence()) {
                result.errors.push(format!("pane split: {error}"));
                return;
            }
            // Let the client consume the split's redraw before the next prefix key; sending
            // splits back to back can drop keys or stall the client on a full output buffer.
            let _ = client.drain_for(Duration::from_millis(120));
        }
        if let Err(error) =
            wait_for_process_count(session.root_pid, panes + 1, Duration::from_secs(5))
        {
            result.benchmarks.push(failed_benchmark(
                "multipane_idle",
                format!("split {panes}: {error} (server plus {panes} pane shells expected)"),
            ));
            return;
        }
        std::thread::sleep(Duration::from_millis(300));
        match quick_cohort_rss(&roots) {
            Ok(rss) => scaling.push((panes, rss)),
            Err(error) => {
                result.errors.push(format!("pane scaling sample: {error}"));
                return;
            }
        }
    }
    let _ = client.drain_for(Duration::from_millis(500));
    std::thread::sleep(Duration::from_secs_f64(profile.settle_seconds));
    let sample_duration = Duration::from_secs_f64(profile.idle_seconds);
    let peak = match sample_cohorts(&roots, sample_duration, interval) {
        Ok((samples, elapsed)) => {
            add_resource_metrics(
                result,
                &format!("multipane_{}_idle", profile.pane_count),
                &samples,
                elapsed,
                "Attached multi-pane session after identical prefix-key split actions.",
            );
            let process_counts = samples
                .iter()
                .map(|sample| sample.process_count as f64)
                .collect();
            result.benchmarks.push(measured_benchmark(
                "multipane_process_count",
                "processes",
                MetricDirection::Neutral,
                process_counts,
                "Observed server/client process roots and descendants; used as a workload validity signal, not a ranking metric.",
            ));
            median_of(samples.iter().map(|sample| sample.cohort_rss_kib as f64))
        }
        Err(error) => {
            result
                .benchmarks
                .push(failed_benchmark("multipane_idle", error));
            return;
        }
    };

    let mut slope = measured_benchmark(
        "pane_memory_slope",
        "KiB/pane",
        MetricDirection::Lower,
        vec![least_squares_slope(&scaling)],
        "Least-squares slope of cohort RSS against pane count from one pane up to the profile pane count; each step is sampled after the new pane shell exists.",
    );
    slope.metadata.insert(
        "cohort_rss_kib_by_pane_count".into(),
        scaling
            .iter()
            .map(|(panes, rss)| format!("{panes}:{rss:.0}"))
            .collect::<Vec<_>>()
            .join(","),
    );
    result.benchmarks.push(slope);

    // Close the added panes by exiting their shells, one at a time, waiting for each shell to
    // disappear. A pane that does not close is a failure; a single retry is recorded, not hidden.
    let mut retries = 0;
    for remaining in (1..profile.pane_count).rev() {
        if let Err(error) = close_one_pane(session.root_pid, client, remaining + 1, &mut retries) {
            result.errors.push(format!("pane close: {error}"));
            result
                .benchmarks
                .push(failed_benchmark("pane_close_recovery", error));
            return;
        }
    }
    let _ = client.drain_for(Duration::from_millis(300));
    std::thread::sleep(Duration::from_secs_f64(profile.settle_seconds));
    let recovery_window = sample_duration.min(Duration::from_secs(10));
    match sample_cohorts(&roots, recovery_window, interval) {
        Ok((samples, _)) => {
            let after = median_of(samples.iter().map(|sample| sample.cohort_rss_kib as f64));
            let base = scaling.first().map(|(_, rss)| *rss).unwrap_or(after);
            let recovered = if peak > base {
                ((peak - after) / (peak - base) * 100.0).max(0.0)
            } else {
                0.0
            };
            let metadata = BTreeMap::from([
                ("one_pane_rss_kib".to_string(), format!("{base:.0}")),
                ("peak_rss_kib".to_string(), format!("{peak:.0}")),
                ("after_close_rss_kib".to_string(), format!("{after:.0}")),
                ("exit_retries".to_string(), retries.to_string()),
            ]);
            let mut recovery = measured_benchmark(
                "pane_close_recovery",
                "%",
                MetricDirection::Higher,
                vec![recovered],
                "Share of the memory added by the extra panes that is returned after their shells exit; 100 means the cohort is back at its one-pane footprint.",
            );
            recovery.metadata = metadata.clone();
            result.benchmarks.push(recovery);
            let mut closed = measured_benchmark(
                "pane_close_rss",
                "KiB",
                MetricDirection::Lower,
                samples
                    .iter()
                    .map(|sample| sample.cohort_rss_kib as f64)
                    .collect(),
                "Cohort RSS after closing the extra panes; compare with the one-pane and peak values in the metadata.",
            );
            closed.metadata = metadata;
            result.benchmarks.push(closed);
        }
        Err(error) => result
            .benchmarks
            .push(failed_benchmark("pane_close_recovery", error)),
    }
}

fn close_one_pane(
    root: u32,
    client: &mut PtyChild,
    target_cohort: usize,
    retries: &mut usize,
) -> Result<(), String> {
    // Drain any pending redraw so the focused pane's shell receives the keystrokes on an idle
    // input line, then close it. A second attempt is made only if the first did not take effect.
    for attempt in 0..2 {
        let _ = client.drain_for(Duration::from_millis(150));
        client.send_line("exit")?;
        if wait_for_process_count_at_most(root, target_cohort, Duration::from_secs(5)).is_ok() {
            std::thread::sleep(Duration::from_millis(200));
            return Ok(());
        }
        if attempt == 0 {
            *retries += 1;
        }
    }
    Err(format!(
        "process cohort did not shrink to {target_cohort} members after typing exit twice; the pane shell did not close"
    ))
}

fn quick_cohort_rss(roots: &[u32]) -> Result<f64, String> {
    let (samples, _) = sample_cohorts(
        roots,
        Duration::from_millis(250),
        Duration::from_millis(100),
    )?;
    Ok(median_of(
        samples.iter().map(|sample| sample.cohort_rss_kib as f64),
    ))
}

fn run_output_workload(
    profile: &ProfileRecord,
    result: &mut ContenderResult,
    client: &mut PtyChild,
) {
    let timeout = if profile.name == "smoke" {
        Duration::from_secs(10)
    } else {
        Duration::from_secs(120)
    };
    let mut completion = Vec::with_capacity(profile.output_iterations);
    let mut ingest = Vec::with_capacity(profile.output_iterations);
    let mut render_bytes = Vec::with_capacity(profile.output_iterations);
    for iteration in 0..profile.output_iterations {
        // Change every payload cell between iterations so a damage-based
        // renderer cannot satisfy later trials by emitting only the marker.
        let payload = output_payload(iteration);
        let marker = format!("UTC_OUTPUT_WORKLOAD_END_{iteration:02}");
        let command = format!(
            "i=0; while [ \"$i\" -lt {} ]; do printf '%08d {}\\n' \"$i\"; i=$((i+1)); done; printf '{}\\n'",
            profile.output_lines,
            payload,
            shell_octal(&marker)
        );
        if let Err(error) = client.clear_observation() {
            result.errors.push(format!("output clear: {error}"));
            return;
        }
        let started = Instant::now();
        if let Err(error) = client.send_line(&command) {
            result.errors.push(format!("output input: {error}"));
            return;
        }
        match client.read_until_text(&marker, timeout) {
            Ok(observation) => {
                let elapsed = started.elapsed().as_secs_f64();
                completion.push(elapsed * 1_000.0);
                ingest.push((profile.output_lines as f64 * 72.0) / elapsed / (1024.0 * 1024.0));
                render_bytes.push(observation.raw_bytes as f64);
            }
            Err(error) => {
                result
                    .benchmarks
                    .push(failed_benchmark("terminal_output_completion", error));
                return;
            }
        }
        // Judge the final screen after the client has finished its frame (outside the sample).
        let _ = client.drain_for(Duration::from_millis(100));
        if let Err(error) =
            verify_output_screen(client.screen(), profile.output_lines, &payload, &marker)
        {
            let error = format!("iteration {}: {error}", iteration + 1);
            result.errors.push(format!("output workload: {error}"));
            result
                .benchmarks
                .push(failed_benchmark("terminal_output_completion", error));
            return;
        }
    }
    result.benchmarks.push(measured_benchmark(
        "terminal_output_completion",
        "ms",
        MetricDirection::Lower,
        completion,
        "Time until the final marker from repeated deterministic shell output bursts becomes visible through the attached client. Every burst is oracle-checked: the ten lines above the marker must be the last ten payload lines, intact and in order.",
    ));
    result.benchmarks.push(measured_benchmark(
        "terminal_output_ingest_rate",
        "MiB/s",
        MetricDirection::Higher,
        ingest,
        "Known 72-byte input lines divided by visible-completion time; coalescing is allowed because final visibility is required.",
    ));
    let mut render = measured_benchmark(
        "client_render_output",
        "bytes",
        MetricDirection::Lower,
        render_bytes,
        "Raw bytes emitted by the app client to the outer PTY during each output workload; lower can indicate effective damage coalescing.",
    );
    render.metadata = BTreeMap::from([
        (
            "input_lines_per_iteration".into(),
            profile.output_lines.to_string(),
        ),
        (
            "output_iterations".into(),
            profile.output_iterations.to_string(),
        ),
        (
            "input_bytes_per_iteration".into(),
            (profile.output_lines * 72).to_string(),
        ),
    ]);
    result.benchmarks.push(render);
}

/// Number of payload lines above the completion marker that the output oracle checks.
const OUTPUT_ORACLE_DEPTH: usize = 10;

/// Characters of each checked tail line (index, space, payload prefix) that must render intact.
/// Short enough to sit on one visual row in any pane wider than 20 cells, so wrapping cannot
/// cause a false failure.
const OUTPUT_ORACLE_PREFIX: usize = 20;

/// Verifies that the burst rendered correctly, without assuming anything about column layout or
/// line wrapping (both products draw sidebar chrome, so payload lines wrap inside a narrow pane).
/// The check is: the completion marker is visible; the last `depth` line indices are each visible
/// above the marker, taking the occurrence nearest the marker, in strictly increasing screen
/// order; and the first `OUTPUT_ORACLE_PREFIX` characters of each of those lines match the
/// expected index and payload exactly.
fn verify_output_screen(
    screen: &Screen,
    lines: usize,
    payload: &str,
    marker: &str,
) -> Result<(), String> {
    let marker_row = screen
        .find(marker)
        .iter()
        .map(|(row, _)| *row)
        .min()
        .ok_or_else(|| {
            format!(
                "oracle: marker {marker} was emitted but is not on the visible screen\n{}",
                screen.dump()
            )
        })?;
    let depth = OUTPUT_ORACLE_DEPTH.min(lines);
    if depth < 3 {
        return Ok(());
    }
    let mut previous_row: Option<usize> = None;
    for offset in (1..=depth).rev() {
        // Oldest checked index first (highest on screen), newest last (just above the marker).
        let index = lines - offset;
        let token = format!("{index:08}");
        let (row, col) = screen
            .find(&token)
            .into_iter()
            .filter(|(row, _)| *row < marker_row)
            .max_by_key(|(row, _)| *row)
            .ok_or_else(|| {
                format!(
                    "oracle: line index {token} from the burst tail is not visible above the marker\n{}",
                    screen.dump()
                )
            })?;
        let expected = format!("{token} {payload}");
        let expected_prefix = &expected[..OUTPUT_ORACLE_PREFIX.min(expected.len())];
        let actual_prefix = screen.slice(row, col, expected_prefix.len());
        if actual_prefix != expected_prefix {
            return Err(format!(
                "oracle: row {row} shows {actual_prefix:?} where the burst tail requires {expected_prefix:?}\n{}",
                screen.dump()
            ));
        }
        if let Some(previous) = previous_row {
            if row <= previous {
                return Err(format!(
                    "oracle: line index {token} is at row {row}, not below the previous tail line at row {previous}; the burst is out of order\n{}",
                    screen.dump()
                ));
            }
        }
        previous_row = Some(row);
    }
    Ok(())
}

fn run_resize_storm(
    profile: &ProfileRecord,
    result: &mut ContenderResult,
    session: &Session,
    client: &mut PtyChild,
) {
    if profile.resize_iterations == 0 {
        return;
    }
    let (cols, rows) = (profile.terminal_cols, profile.terminal_rows);
    let roots = [session.root_pid, client.pid()];
    let outcome = (|| -> Result<(f64, f64, f64, BTreeMap<String, String>), String> {
        let baseline = probe_pane_size(client, "SZ0")?;
        let before = cohort_cpu_seconds(&roots)?;
        // A deterministic burst of distinct geometries, none of which is the final one.
        for index in 0..profile.resize_iterations {
            let storm_cols = cols.saturating_sub(10 + ((index * 13) % 40) as u16).max(40);
            let storm_rows = rows.saturating_sub(4 + ((index * 7) % 16) as u16).max(10);
            client.resize(storm_cols, storm_rows)?;
            std::thread::sleep(Duration::from_millis(10));
        }
        let final_resize = Instant::now();
        client.resize(cols, rows)?;
        // Settle: the pane's own PTY must report the baseline size again. Each probe is a real
        // shell round trip, so the settle time includes one probe latency for both products. The
        // deadline is generous because a busy renderer may still be draining the resize burst.
        let deadline = final_resize + Duration::from_secs(20);
        let mut probes = 0;
        let settled = loop {
            probes += 1;
            let tag = format!("SZ{probes}");
            let size = probe_pane_size(client, &tag)?;
            if size == baseline {
                break final_resize.elapsed();
            }
            if Instant::now() >= deadline {
                return Err(format!(
                    "pane size {size:?} did not return to {baseline:?} within 20 s of the final resize after {probes} probes"
                ));
            }
            client.drain_for(Duration::from_millis(200))?;
        };
        let after = cohort_cpu_seconds(&roots)?;
        client.drain_for(Duration::from_millis(50))?;
        let retained_rss = quick_cohort_rss(&roots)?;
        let final_tag = format!("SZ{probes}:");
        if client.screen().count(&final_tag) == 0 {
            return Err(format!(
                "oracle: the settled size probe {final_tag} is not on the visible screen\n{}",
                client.screen().dump()
            ));
        }
        let metadata = BTreeMap::from([
            (
                "resize_iterations".to_string(),
                profile.resize_iterations.to_string(),
            ),
            ("settle_probes".to_string(), probes.to_string()),
            (
                "pane_size_rows_cols".to_string(),
                format!("{} {}", baseline.0, baseline.1),
            ),
            (
                "scrollback_lines_before_storm".to_string(),
                (profile.output_lines * profile.output_iterations).to_string(),
            ),
        ]);
        Ok((
            settled.as_secs_f64() * 1_000.0,
            (after - before).max(0.0) * 1_000.0,
            retained_rss,
            metadata,
        ))
    })();
    match outcome {
        Ok((settle_ms, cpu_ms, retained_rss, metadata)) => {
            let mut settle = measured_benchmark(
                "resize_storm_settle",
                "ms",
                MetricDirection::Lower,
                vec![settle_ms],
                "Time from the final resize of a rapid resize burst, applied to a pane holding the scrollback left by the output workload, until the pane's PTY reports the original size again through a shell probe and that probe is visible.",
            );
            settle.metadata = metadata.clone();
            result.benchmarks.push(settle);
            let mut cpu = measured_benchmark(
                "resize_storm_cpu",
                "ms CPU",
                MetricDirection::Lower,
                vec![cpu_ms],
                "CPU time consumed by the server and client cohorts across the resize burst over the output-workload scrollback and its settle probes.",
            );
            cpu.metadata = metadata.clone();
            result.benchmarks.push(cpu);
            let mut rss = measured_benchmark(
                "resize_storm_rss",
                "KiB",
                MetricDirection::Lower,
                vec![retained_rss],
                "Server plus client cohort RSS right after the resize storm settles; compare with foreground_idle_cohort_rss to see how much memory the reflow work retained.",
            );
            rss.metadata = metadata;
            result.benchmarks.push(rss);
        }
        Err(error) => {
            result.errors.push(format!("resize storm: {error}"));
            result
                .benchmarks
                .push(failed_benchmark("resize_storm_settle", error));
        }
    }
}

/// Asks the pane shell for its PTY size (`stty size`) behind a non-echoable tag and parses the
/// `TAG:rows cols:` line from the modelled screen. The screen, not the raw stream, is used because
/// consecutive probes land on the same bottom row of a scrolling pane and a damage-based renderer
/// then emits only the cells that changed.
fn probe_pane_size(client: &mut PtyChild, tag: &str) -> Result<(u16, u16), String> {
    let prefix = format!("{tag}:");
    client.clear_observation()?;
    let command = format!("printf '{}%s:\\n' \"$(stty size)\"", shell_octal(&prefix));
    client.send_line(&command)?;
    client.read_until_screen(&prefix, Duration::from_secs(5))?;
    let deadline = Instant::now() + Duration::from_secs(1);
    loop {
        if let Some(size) = parse_size_probe(client.screen(), &prefix) {
            return Ok(size);
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "could not parse the pane size probe {prefix} from the visible screen\n{}",
                client.screen().dump()
            ));
        }
        client.drain_for(Duration::from_millis(20))?;
    }
}

/// Finds `PREFIXrows cols:` on the visible screen and returns (rows, cols).
fn parse_size_probe(screen: &Screen, prefix: &str) -> Option<(u16, u16)> {
    screen.find(prefix).into_iter().find_map(|(row, col)| {
        let text = screen.slice(row, col + prefix.chars().count(), 16);
        let end = text.find(':')?;
        let mut fields = text[..end].split_whitespace();
        let rows = fields.next()?.parse().ok()?;
        let cols = fields.next()?.parse().ok()?;
        Some((rows, cols))
    })
}

fn cohort_cpu_seconds(roots: &[u32]) -> Result<f64, String> {
    let snapshot = ProcessSnapshot::collect()?;
    snapshot
        .metrics_many(roots)
        .map(|metrics| metrics.cohort_cpu_seconds)
        .ok_or_else(|| format!("one of root processes {roots:?} exited"))
}

fn run_multiclient(
    profile: &ProfileRecord,
    result: &mut ContenderResult,
    session: &Session,
    client: &mut PtyChild,
) {
    if profile.extra_clients == 0 {
        return;
    }
    let total = profile.extra_clients + 1;
    let prefix = format!("multiclient_{total}");
    let mut extras: Vec<PtyChild> = Vec::new();
    for index in 0..profile.extra_clients {
        let step = (index + 1) as u16;
        let cols = profile.terminal_cols.saturating_sub(20 * step).max(60);
        let rows = profile.terminal_rows.saturating_sub(6 * step).max(16);
        match session.attach_with(cols, rows) {
            Ok(extra) => extras.push(extra),
            Err(error) => {
                result
                    .errors
                    .push(format!("multi-client attach {}: {error}", index + 1));
                result.benchmarks.push(failed_benchmark(
                    format!("{prefix}_input_to_visible"),
                    format!("extra client {} did not attach: {error}", index + 1),
                ));
                detach_all(session, extras);
                return;
            }
        }
    }
    for extra in &mut extras {
        let _ = extra.drain_for(Duration::from_millis(500));
    }
    let _ = client.drain_for(Duration::from_millis(500));

    match latency_samples(profile.latency_iterations, client) {
        Ok(samples) => {
            // Every attached client must also show the final marker: shared sessions that only
            // update one client have not completed the workload.
            let marker = latency_marker(profile.latency_iterations.saturating_sub(1));
            let mut mismatch = None;
            for (index, extra) in extras.iter_mut().enumerate() {
                let _ = extra.drain_for(Duration::from_millis(200));
                if extra.screen().count(&marker) == 0 {
                    mismatch = Some(format!(
                        "oracle: extra client {} never showed marker {marker}\n{}",
                        index + 1,
                        extra.screen().dump()
                    ));
                    break;
                }
            }
            match mismatch {
                None => result.benchmarks.push(measured_benchmark(
                    format!("{prefix}_input_to_visible"),
                    "ms",
                    MetricDirection::Lower,
                    samples,
                    format!("Input-to-visible latency on the primary client while {} additional clients of smaller geometry are attached; the final marker must be visible on every client.", profile.extra_clients),
                )),
                Some(error) => {
                    result.errors.push(format!("multi-client: {error}"));
                    result
                        .benchmarks
                        .push(failed_benchmark(format!("{prefix}_input_to_visible"), error));
                }
            }
        }
        Err(error) => {
            result.errors.push(format!("multi-client latency: {error}"));
            result.benchmarks.push(failed_benchmark(
                format!("{prefix}_input_to_visible"),
                error,
            ));
        }
    }

    std::thread::sleep(Duration::from_secs_f64(profile.settle_seconds));
    let mut roots = vec![session.root_pid, client.pid()];
    roots.extend(extras.iter().map(PtyChild::pid));
    match sample_cohorts(
        &roots,
        Duration::from_secs_f64(profile.idle_seconds),
        Duration::from_millis(profile.sample_interval_ms),
    ) {
        Ok((samples, elapsed)) => add_resource_metrics(
            result,
            &format!("{prefix}_idle"),
            &samples,
            elapsed,
            "Server, every attached client, pane shells, and descendants while several clients stay attached to one session.",
        ),
        Err(error) => result
            .benchmarks
            .push(failed_benchmark(format!("{prefix}_idle"), error)),
    }
    detach_all(session, extras);
    let _ = client.drain_for(Duration::from_millis(300));
}

fn detach_all(session: &Session, extras: Vec<PtyChild>) {
    for mut extra in extras {
        let _ = extra.send(session.detach_sequence());
        let _ = extra.drain_for(Duration::from_millis(200));
        extra.terminate();
    }
}

fn run_restart(result: &mut ContenderResult, session: &mut Session, expected_marker: &str) {
    let elapsed = match session.restart() {
        Ok(elapsed) => elapsed,
        Err(error) => {
            result.errors.push(format!("restart: {error}"));
            result
                .benchmarks
                .push(failed_benchmark("restart_ready", error));
            return;
        }
    };
    let mut metadata = BTreeMap::new();
    if result.adapter == "tmux" {
        metadata.insert("native_disk_restoration".into(), "not_applicable".into());
        metadata.insert("restoration_note".into(), "Stock tmux has no native disk restoration; restarting creates a fresh session. Plugins are outside this baseline.".into());
    }
    match session.attach() {
        Ok(mut client) => {
            let _ = client.drain_for(Duration::from_millis(1500));
            let restored = client.screen().count(expected_marker) > 0;
            metadata.insert(
                "prior_output_visible_after_restart".into(),
                if restored { "yes" } else { "no" }.to_string(),
            );
            let shells = ProcessSnapshot::collect()
                .map(|snapshot| snapshot.descendants_including(session.root_pid).len() - 1)
                .unwrap_or(0);
            metadata.insert("pane_shells_after_restart".into(), shells.to_string());
            let _ = client.send(session.detach_sequence());
            let _ = client.drain_for(Duration::from_millis(200));
            client.terminate();
        }
        Err(error) => {
            result.errors.push(format!("restart attach: {error}"));
            metadata.insert(
                "prior_output_visible_after_restart".into(),
                "unknown".into(),
            );
        }
    }
    let mut restart = measured_benchmark(
        "restart_ready",
        "ms",
        MetricDirection::Neutral,
        vec![elapsed.as_secs_f64() * 1_000.0],
        "Restarting the stopped session with the product's own start command until the pane-listing probe succeeds. Context only: the products restore different state, so see the metadata for whether the previous burst output was visible again.",
    );
    restart.metadata = metadata;
    result.benchmarks.push(restart);
    if let Err(error) = session.stop() {
        result.errors.push(format!("restart teardown: {error}"));
    }
}

fn median_of(values: impl Iterator<Item = f64>) -> f64 {
    let mut values: Vec<f64> = values.collect();
    if values.is_empty() {
        return 0.0;
    }
    values.sort_by(f64::total_cmp);
    let middle = values.len() / 2;
    if values.len() % 2 == 0 {
        (values[middle - 1] + values[middle]) / 2.0
    } else {
        values[middle]
    }
}

fn least_squares_slope(points: &[(usize, f64)]) -> f64 {
    if points.len() < 2 {
        return 0.0;
    }
    let count = points.len() as f64;
    let mean_x = points.iter().map(|(x, _)| *x as f64).sum::<f64>() / count;
    let mean_y = points.iter().map(|(_, y)| *y).sum::<f64>() / count;
    let numerator: f64 = points
        .iter()
        .map(|(x, y)| (*x as f64 - mean_x) * (y - mean_y))
        .sum();
    let denominator: f64 = points
        .iter()
        .map(|(x, _)| (*x as f64 - mean_x).powi(2))
        .sum();
    if denominator == 0.0 {
        0.0
    } else {
        numerator / denominator
    }
}

fn add_resource_metrics(
    result: &mut ContenderResult,
    prefix: &str,
    samples: &[CohortMetrics],
    elapsed: Duration,
    note: &str,
) {
    if let Some(cpu) = cpu_percent(samples, elapsed, false) {
        result.benchmarks.push(measured_benchmark(
            format!("{prefix}_root_cpu"),
            "% core",
            MetricDirection::Lower,
            vec![cpu],
            note,
        ));
    }
    if let Some(cpu) = cpu_percent(samples, elapsed, true) {
        result.benchmarks.push(measured_benchmark(
            format!("{prefix}_cohort_cpu"),
            "% core",
            MetricDirection::Lower,
            vec![cpu],
            note,
        ));
    }
    result.benchmarks.push(measured_benchmark(
        format!("{prefix}_root_rss"),
        "KiB",
        MetricDirection::Lower,
        samples
            .iter()
            .map(|sample| sample.root_rss_kib as f64)
            .collect(),
        note,
    ));
    result.benchmarks.push(measured_benchmark(
        format!("{prefix}_cohort_rss"),
        "KiB",
        MetricDirection::Lower,
        samples
            .iter()
            .map(|sample| sample.cohort_rss_kib as f64)
            .collect(),
        note,
    ));
}

fn shell_octal(text: &str) -> String {
    text.bytes().map(|byte| format!("\\{byte:03o}")).collect()
}

fn latency_marker(iteration: usize) -> String {
    // Adjacent trials must change every cell. A damage-based renderer may otherwise emit only
    // the one changed digit, making a raw outer-PTY observer unable to prove the full new marker
    // is visible even though the terminal screen is correct.
    let byte = b'A' + (iteration % 26) as u8;
    std::iter::repeat_n(char::from(byte), 8).collect()
}

fn output_payload(iteration: usize) -> String {
    const PAYLOAD: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ";
    PAYLOAD
        .iter()
        .cycle()
        .skip(iteration % PAYLOAD.len())
        .take(PAYLOAD.len())
        .map(|byte| char::from(*byte))
        .collect()
}

fn rotated_order(count: usize, seed: u64) -> Vec<usize> {
    let mut order: Vec<usize> = (0..count).collect();
    if count > 1 {
        order.rotate_left((seed as usize) % count);
    }
    order
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marker_encoding_does_not_echo_plain_marker() {
        let encoded = shell_octal("MARK");
        assert_eq!(encoded, "\\115\\101\\122\\113");
        assert!(!encoded.contains("MARK"));
    }

    #[test]
    fn latency_markers_change_every_visible_cell() {
        for iteration in 1..100 {
            let previous = latency_marker(iteration - 1);
            let current = latency_marker(iteration);
            assert_eq!(current.len(), previous.len());
            assert!(current
                .bytes()
                .zip(previous.bytes())
                .all(|(left, right)| left != right));
        }
    }

    #[test]
    fn output_payloads_change_every_payload_cell() {
        for iteration in 1..62 {
            let previous = output_payload(iteration - 1);
            let current = output_payload(iteration);
            assert_eq!(current.len(), 62);
            assert!(current
                .bytes()
                .zip(previous.bytes())
                .all(|(left, right)| left != right));
        }
    }

    #[test]
    fn output_oracle_requires_intact_ordered_tail() {
        let payload = output_payload(0);
        // Narrow screen so payload lines wrap, exactly like the products' sidebar chrome.
        let mut screen = Screen::new(40, 24);
        let mut text = String::new();
        for index in 9_980..10_000 {
            text.push_str(&format!("{index:08} {payload}\r\n"));
        }
        text.push_str("UTC_OUTPUT_WORKLOAD_END_00\r\nsh$ ");
        screen.feed(text.as_bytes());
        assert!(
            verify_output_screen(&screen, 10_000, &payload, "UTC_OUTPUT_WORKLOAD_END_00").is_ok(),
            "{}",
            screen.dump()
        );
        let missing = verify_output_screen(&screen, 10_000, &payload, "UTC_OUTPUT_WORKLOAD_END_01")
            .unwrap_err();
        assert!(missing.contains("not on the visible screen"));
        // A dropped tail line fails: render a burst that skips index 00009995.
        let mut gap = Screen::new(40, 24);
        let mut gap_text = String::new();
        for index in 9_980..10_000 {
            if index == 9_995 {
                continue;
            }
            gap_text.push_str(&format!("{index:08} {payload}\r\n"));
        }
        gap_text.push_str("UTC_OUTPUT_WORKLOAD_END_00\r\nsh$ ");
        gap.feed(gap_text.as_bytes());
        let error =
            verify_output_screen(&gap, 10_000, &payload, "UTC_OUTPUT_WORKLOAD_END_00").unwrap_err();
        assert!(error.contains("not visible above the marker"), "{error}");
        // Garbled payload text next to an intact index fails.
        let mut garbled = Screen::new(40, 24);
        let mut garbled_text = String::new();
        for index in 9_980..10_000 {
            let line = if index == 9_997 {
                format!("{index:08} 0123XXXXXX{}", &payload[10..])
            } else {
                format!("{index:08} {payload}")
            };
            garbled_text.push_str(&line);
            garbled_text.push_str("\r\n");
        }
        garbled_text.push_str("UTC_OUTPUT_WORKLOAD_END_00\r\nsh$ ");
        garbled.feed(garbled_text.as_bytes());
        let error = verify_output_screen(&garbled, 10_000, &payload, "UTC_OUTPUT_WORKLOAD_END_00")
            .unwrap_err();
        assert!(error.contains("burst tail requires"), "{error}");
    }

    #[test]
    fn latency_oracle_rejects_stale_markers() {
        let mut screen = Screen::new(40, 4);
        screen.feed(b"AAAAAAAA\r\nBBBBBBBB");
        assert!(verify_latency_screen(&screen, "BBBBBBBB", None).is_ok());
        assert!(verify_latency_screen(&screen, "BBBBBBBB", Some("AAAAAAAA")).is_err());
        assert!(verify_latency_screen(&screen, "CCCCCCCC", None).is_err());
    }

    #[test]
    fn size_probe_parsing_and_statistics() {
        let mut screen = Screen::new(40, 3);
        screen.feed(b"junk SZ3:40 118:\r\nsh$ ");
        assert_eq!(parse_size_probe(&screen, "SZ3:"), Some((40, 118)));
        let mut partial = Screen::new(40, 3);
        partial.feed(b"SZ3:40 11");
        assert_eq!(parse_size_probe(&partial, "SZ3:"), None);
        assert_eq!(parse_size_probe(&screen, "SZ4:"), None);
        assert_eq!(median_of([3.0, 1.0, 2.0].into_iter()), 2.0);
        assert_eq!(median_of([4.0, 1.0, 3.0, 2.0].into_iter()), 2.5);
        let slope = least_squares_slope(&[(1, 100.0), (2, 150.0), (3, 200.0), (4, 250.0)]);
        assert!((slope - 50.0).abs() < 1e-9);
        assert_eq!(least_squares_slope(&[(1, 5.0)]), 0.0);
    }

    #[test]
    fn trial_order_is_seeded() {
        assert_eq!(rotated_order(3, 1), vec![1, 2, 0]);
    }
}
