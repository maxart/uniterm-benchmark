# Comparison methodology

## Goal

This document defines the tool. The repository contains no measured comparison results or product review evidence.

The benchmark answers a narrow, useful question: on the same host, with the same terminal geometry and shell workload, what resources and time do the reviewed Uniterm, Herdr and optional tmux binaries require for equivalent multiplexer operations?

Feature breadth is outside the performance index. The configuration templates carry no comparative product findings.

## Fairness rules

1. Use release binaries from recorded source commits.
2. Record binary path, size, version output, and SHA-256.
3. Run on an otherwise quiet host with fixed power and thermal settings.
4. Give all products private, empty HOME and XDG directories.
5. Use `/bin/sh`, `TERM=xterm-256color`, the same locale, and identical terminal dimensions.
6. Disable Herdr's default network checks during timing; score their default privacy behavior separately.
7. Use product control commands only for setup, readiness, status, and teardown. Readiness and the control-latency probe are the same semantic operation for all contenders: a fresh CLI process listing the session's panes through the product socket, which exits non-zero when no server answers.
8. Send latency and output work through an attached pseudo-terminal for all contenders.
9. Before sampling detached idle, attach one client, wait for the first pane shell, and detach. Uniterm starts a pane shell with the Workspace while Herdr's headless server has no pane until a client attaches; without this step one server would idle with a live shell and the other without.
10. Pin Herdr's headless render grid (`[server] headless_cols/rows`) to the attached geometry so a detached Herdr server does not render at a different size than the profile.
11. Include the identical child shell cost in all end-to-end latency values.
12. Measure root processes and full cohorts separately.
13. Reverse contender order on alternating startup trials and rotate full-suite order with the configured seed.
14. Treat a missing marker, failed readiness response, premature process exit, or invalid pane cohort as a failed measurement, never as a slow or zero result.

## Scenarios and metrics

| Scenario | Metric | Unit | Direction | Why it matters |
| --- | --- | --- | --- | --- |
| Binary artifact | `binary_size` | bytes | lower | Distribution and page-cache context. |
| Fresh isolated lifecycle | `server_startup_ready` | ms | lower | Product start command until the first successful pane-listing probe; PID discovery is outside the timed window. |
| Running isolated server | `control_command_latency` | ms | lower | Fresh CLI process listing panes through the socket; automation overhead of process start plus one IPC round trip. |
| Detached after one attach/detach, settled | `daemon_idle_*_cpu` | % of one core | lower | Background energy behavior with exactly one pane shell. |
| Detached after one attach/detach, settled | `daemon_idle_*_rss` | KiB | lower | Persistent background footprint with exactly one pane shell. |
| Attached, settled | `foreground_idle_*_cpu` | % of one core | lower | Idle render/event-loop behavior. |
| Attached, settled | `foreground_idle_*_rss` | KiB | lower | Server plus visible client footprint. |
| One visible pane | `terminal_input_to_visible` | ms | lower | End-to-end interactive response. |
| One visible pane | `terminal_output_completion` | ms | lower | Burst completion through parse, grid, render, and client. |
| One visible pane | `terminal_output_ingest_rate` | MiB/s | higher | Known input bytes divided by visible completion time. |
| One visible pane | `client_render_output` | bytes | lower | Damage tracking and coalescing context. |
| N attached panes | `multipane_N_idle_*` | % / KiB | lower | Scaling after identical prefix-key splits. |
| N attached panes | `multipane_process_count` | processes | neutral | Workload validity, not a quality metric. |
| Post-workload | `isolated_state_size` | bytes | neutral | Persistence context; products retain different semantics. |
| Graceful stop | `server_shutdown` | ms | lower | Teardown and persistence completion. |
| Pane scaling 1..N | `pane_memory_slope` | KiB/pane | lower | Cohort RSS growth per added pane, fit by least squares; context. |
| Close added panes | `pane_close_recovery` | % | higher | Share of added pane memory returned after the shells exit; context. |
| Close added panes | `pane_close_rss` | KiB | lower | Cohort RSS after the added panes close; context. |
| Resize burst over the burst scrollback | `resize_storm_settle` | ms | lower | Time until the pane PTY reports the original size after a rapid resize burst; context. |
| Resize burst over the burst scrollback | `resize_storm_cpu` | ms CPU | lower | Cohort CPU consumed across the resize burst and its settle; context. |
| Resize burst over the burst scrollback | `resize_storm_rss` | KiB | lower | Cohort RSS right after the storm settles, showing memory retained by reflow; context. |
| Several clients, fresh session | `multiclient_N_input_to_visible` | ms | lower | Primary-client latency with N-1 extra clients attached at smaller sizes; context. |
| Several clients, fresh session | `multiclient_N_idle_*` | % / KiB | lower | Cohort CPU/RSS while several clients stay attached; context. |
| Restart | `restart_ready` | ms | neutral | Stop then restart until the pane probe answers; context, because restored state differs. |

Root metrics sum the named server and attached-client roots when present.
Cohort metrics additionally include their transitive descendants, deduplicated by PID.
The server cohort therefore includes pane shells; the attached client is added as a second root because it is not a server descendant.

CPU is calculated from the change in cumulative process CPU time divided by elapsed wall time.
`ps` supplies the PID tree and RSS everywhere. On Linux the CPU time comes from `/proc/<pid>/stat` utime+stime (10 ms ticks) because procps `ps` reports whole seconds, which cannot resolve idle CPU in a 30 s window; other platforms keep the `ps` value. The source and resolution are recorded in `host.cpu_time_source`.
The percentage is one-core percentage: 100 means one fully occupied logical core, regardless of machine core count.
RSS is sampled repeatedly and reports median, p95, mean, minimum, maximum, and standard deviation in JSON.

## Screen correctness oracle

The harness replays every byte a product client writes to the outer pseudo-terminal into a small
VT100/xterm screen model (`src/screen.rs`: cursor movement, erase, scroll regions, insert/delete,
alternate screen, save/restore, UTF-8). After each latency trial it requires the new marker to be
on the modelled screen and the previous marker to be gone. After each output burst it requires the
completion marker to be visible, the last ten line indices to be present above it in increasing
screen order (taking, for each index, the occurrence nearest the marker), and the first twenty
characters of each of those lines (index, space, payload prefix) to match exactly. This tolerates
the sidebar chrome and line wrapping both products draw while still catching dropped, reordered,
or garbled lines. A wrong or incomplete final screen fails the metric; correctness gates
performance rather than scoring in it.
The model is a deliberately conservative oracle, not a full terminal emulator.

## Extended validation scenarios

These run through the same attached pseudo-terminal for all contenders and are reported as context,
not folded into the balanced index:

- Pane scaling grows one pane at a time up to the profile pane count, sampling cohort RSS after each
  new pane shell exists, and fits a per-pane memory slope.
- Memory recovery closes the added panes by exiting their shells one at a time, waiting for each to
  disappear, and reports the share of the added memory that is returned.
- The resize storm runs in the live session after the output workload, so the pane holds that
  workload's scrollback (profile output lines times iterations). It applies a deterministic burst
  of distinct window sizes, none of them the final one, then measures the time until the pane PTY
  reports the original size again, the CPU spent, and the cohort RSS retained afterwards. This is
  deliberately a reflow-under-load test, identical for all contenders.
- The multi-client scenario starts a fresh session (like the multi-pane scenario) so retention from
  the burst and the storm cannot bias it, attaches additional clients of smaller geometry, and
  measures primary input-to-visible latency and the whole cohort's idle CPU/RSS, requiring the
  final marker on every client.
- Restart stops and restarts the session and records readiness time and whether prior output is
  visible again; it is neutral because the products restore different state.

## Deterministic terminal workloads

Latency iterations submit a POSIX `printf` command that clears/homes the viewport and writes an
eight-cell marker as octal escapes. The literal marker is absent from the echoed command, so it
cannot be observed before shell execution. Every marker cell differs from the preceding iteration;
this lets the outer PTY prove the complete new marker is visible whether a client emits a full frame
or only changed cells. Resetting the viewport also keeps every iteration out of the scroll path.

The output workload repeatedly emits fixed 72-byte lines with a monotonically increasing index, followed by a unique non-echoable completion marker for each iteration.
Every iteration changes all payload cells so a damage-based renderer cannot reuse the preceding iteration's final screen.
Completion requires marker bytes to pass through the product and reach the outer PTY.
Products may coalesce intermediate frames; that is legitimate because final visible state is still required.
Smoke, standard, and marketing collect 2, 5, and 10 output samples respectively so a single scheduler disturbance cannot decide the throughput comparison.

The output workload runs in a one-pane session at the configured dimensions.
The multi-pane resource scenario starts from a separate fresh session so output scrollback and allocator retention cannot bias pane-scaling memory.

## Ranking

The balanced performance index uses the geometric mean of per-metric ratios for exactly these core metrics:

1. `server_startup_ready`
2. `control_command_latency`
3. `daemon_idle_cohort_cpu`
4. `daemon_idle_cohort_rss`
5. `foreground_idle_cohort_cpu`
6. `foreground_idle_cohort_rss`
7. `terminal_input_to_visible`
8. `terminal_output_completion`

The best contender receives ratio 1 for each metric.
Other ratios are best/value for lower-is-better metrics and value/best for higher-is-better metrics, bounded at 0.01 before the geometric mean.
Values within one percent of the best count as ratio 1, matching the tie rule in the tables.
CPU metrics are floored at 0.1 percent of a core before the ratio is formed: below that level the reading is scheduler-tick and background noise, and without the floor a 0.00 versus 0.03 percent pair would become a 100x ratio that dominates the geometric mean.
If any contender lacks any core metric, no balanced ranking is produced. A core metric whose screen
oracle failed is absent, so a contender that renders incorrectly cannot receive a balanced index.
The extended validation scenarios above are reported with per-metric winners but are never part of
the index.

Individual result tables treat results within 1 percent as a tie.
CPU zeros in short runs therefore do not become a claimed win.

Security and privacy use separate evidence-weighted checklist scores.
They are never combined with performance.

## tmux adapter

Stock tmux is an optional third contender (`adapter = "tmux"`), with the same core metric definitions, profile values, sampling windows, shell workload, screen oracles, order rotation and complete-cohort accounting. No tmux API shortcuts are used for terminal latency or output completion. The initial implementation is validated against official tmux 3.7c; other versions require a fresh smoke run.

Every invocation passes `-S <private-runtime>/tmux.sock -f <private-config>/tmux-benchmark.conf`. Environment inheritance is cleared, including `TMUX`, and the owner-only runtime lives inside the trial's private HOME/XDG tree. The explicit configuration replaces system/user configuration. It sets only:

```tmux
set -g default-shell /bin/sh
set -g default-command 'exec /bin/sh'
set -g default-terminal xterm-256color
bind-key % split-window -h -f \; select-layout tiled
```

The shell command prevents tmux's default login-shell invocation and leaves exactly one `/bin/sh` per pane. The inner TERM is pinned for this common ANSI workload; this is a controlled benchmark setting, not general tmux configuration advice (tmux normally advertises a screen/tmux terminal type). Status lines, history limits, redraw scheduling, escape-time and multi-client size selection retain tmux defaults. Outer PTY geometry is identical across contenders; product chrome can leave different inner pane heights, which the resize probe observes rather than assumes.

Startup runs `new-session -d -s NAME -x COLS -y ROWS -P -F '#{pid}'`, followed by a fresh `list-panes -t NAME` readiness probe through that socket. PID parsing occurs after the timed window and never searches for an unrelated tmux server. Failed startup attempts issue `kill-server` only on the private socket. Control latency uses the same fresh pane-listing command. Attach uses `attach-session -t NAME`; detach sends Ctrl-B d; shutdown uses `kill-server`. Detached idle still follows one attach/detach, even though tmux already creates its first shell at startup.

Pane scaling sends Ctrl-B % through the attached PTY. Its binding splits the full window horizontally and tiles the resulting panes, avoiding a layout-dependent minimum-size failure from repeatedly halving the active pane. This setup is outside core timing; it creates one additional live shell per action. The same pane-count/cohort validity checks and shell-exit recovery workload apply. No scrollback, history, or status settings are tuned to improve tmux results. Multi-client probes retain the common smaller client geometries and require the final marker on every client.

All 36 existing metrics apply. Units, direction, validity conditions and the eight-metric index are unchanged. `restart_ready` (milliseconds, neutral/context) measures a fresh session after graceful tmux server shutdown, not restoration of a saved session. Its metadata separately records observed prior-output visibility and `native_disk_restoration = "not_applicable"`. Markdown renders native restoration as **N/A** only after a successful restart measurement; restart failure remains FAILED. Stock tmux has no built-in session/scrollback disk restoration; third-party plugins are excluded. `isolated_state_size` remains an actual byte count, possibly zero, and never contributes to the performance index.

Rust source/test lines, Rust lexical unsafe/unwrap/API counts and Cargo dependencies are inapplicable to a C implementation. Schemas 6 and 7 store these as `null`, rendered N/A; a measured Rust count of zero stays zero. Markdown documentation counts cover `.md`/`.mdx`, not tmux's man page, so they cannot establish relative documentation completeness. This collector makes no claim about C implementation size, memory safety or dependency breadth. The same assurance rubric applies; unassessed criteria remain unknown, not favorable inferred scores.

The report reader accepts schema 5 integer counts, schema 6 nullable counts and schema 7 sanitized output. Original inputs are validated before the privacy filter runs; imports always produce sanitized output. Mixed-input-schema merges and changed adapter identities are rejected in addition to the existing profile, contender-set and source-commit checks. Historical two-product runs cannot be supplemented with a separately timed tmux run to form a three-product index. Run the complete set together, and collect three marketing trials per host before publishing idle-resource claims.

Adapter references: the [official tmux 3.7c manual](https://github.com/tmux/tmux/blob/3.7c/tmux.1), [command dispatch](https://github.com/tmux/tmux/blob/3.7c/cmd.c), and [server lifecycle](https://github.com/tmux/tmux/blob/3.7c/server.c).

## Required OS matrix

For a public cross-platform comparison, collect at minimum:

- Native Linux x86_64 on a named distribution/kernel.
- WSL2 on a named Windows build, WSL kernel, distribution, and CPU.
- macOS Apple Silicon on a named macOS and hardware version.
- Intel macOS only if it is a supported marketing target for both supplied binaries.

Use at least three `marketing` runs per host.
Record whether the machine was on AC power, its power mode, other foreground load, and terminal emulator.
The sanitized report captures OS, numeric kernel version, architecture, a generic host label, WSL detection, profile, commits, dirtiness, and binary hashes; operational notes should accompany the published report.

## Additional tests recommended before strong claims

The implemented suite is the common, automatable baseline.
These extensions are recommended as follow-on modules because they require product-specific semantic verification:

- 20+ pane/session steady-state and proportional memory slope.
- Memory recovery after closing half and then all added panes.
- Resize storms with final screen oracle verification.
- Unicode, combining mark, wide glyph, hyperlink, alternate-screen, mouse, and scrollback correctness under load.
- Slow/stalled client backpressure and bounded queue memory.
- Multiple simultaneous clients at different dimensions.
- Crash/restart restoration time and restored-state completeness.
- Agent state detection latency from cooperative hook, screen evidence, and process exit.
- Large Git repository invalidation latency and idle work.
- Socket fuzzing, malformed frame limits, permissions, symlink/path handling, and peer identity.
- Dependency vulnerability and license scanning using pinned advisory databases.
- Power/energy collection using platform-native tools (`perf`/RAPL, Instruments/powermetrics, and Windows energy tooling) with appropriate privileges.

Correctness must gate performance in those modules.
A renderer that drops required state is not faster; it is incorrect.

## Output privacy

Schema 7 applies the allowlist described in [PRIVACY.md](PRIVACY.md). Host identity, paths, timestamps, raw diagnostics, terminal contents, arbitrary notes and review text are omitted. Numeric measurements, failure statuses, source revisions and artifact hashes are preserved. Custom metadata cannot bypass the filter. `keep_workdirs = true` is unsupported, and the server's stdout/stderr are discarded rather than retained in a diagnostic file.

This is an intentional output-contract change, not a change to the profile workload or ranking formula. Failed or incomplete startup trial sets remain failures. Operators who publish their own results should describe the workload and retained host context, but this tool repository distributes no evidence bundle.
