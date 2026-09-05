# AGENTS.md

## Scope

These instructions apply to the entire `ut-compare` repository.

This project is a reproducible performance and assurance comparison harness for Uniterm (`ut`) and Herdr. It targets native Linux, WSL-as-Linux, and macOS. It does not target native Windows.

The primary goal is a fair, inspectable comparison. Do not optimize the harness to favor either contender, conceal incomplete measurements, or turn feature differences into performance penalties.

## Sources of truth

Read these before changing benchmark behavior or published claims:

- `README.md`: supported workflows and user-facing commands.
- `docs/METHODOLOGY.md`: fairness rules, metric definitions, ranking, and publication requirements.
- `docs/ASSURANCE.md`: neutral criterion definitions. No application reviews or results belong in this repository.
- `comparison.toml`: neutral configuration template and assurance criteria.

Application sources and binaries are selected in ignored local configuration files. Treat product repositories as read-only unless the user explicitly authorizes changes. Do not include source clones, local evidence, generated reports, or identifying paths in the public repository.

## Repository layout

- `src/main.rs`: CLI dispatch and atomic JSON/Markdown output.
- `src/config.rs`: configuration loading, validation, path resolution, and profiles.
- `src/model.rs`: versioned serialized report schema and metric models.
- `src/adapters.rs`: Uniterm, Herdr and tmux lifecycle/control adapters.
- `src/pty.rs`: real Unix PTY creation, observation, and cleanup.
- `src/process.rs`: portable `ps` process-tree CPU/RSS collection and host metadata.
- `src/audit.rs`: artifact, source, and static engineering context collection.
- `src/runner.rs`: trial ordering and benchmark scenario orchestration.
- `src/report.rs`: merge validation, ranking, assurance scoring, and Markdown generation.
- `reports/`: generated JSON/Markdown evidence; intentionally ignored.
- `.artifacts/`: local release binaries used for measurements; intentionally ignored.
- `comparison*.local.toml`: machine-local binary paths; intentionally ignored.

## Fairness invariants

Preserve all of these unless the methodology is deliberately revised and documented:

1. Both contenders receive equivalent black-box terminal workloads through a real attached PTY.
2. Product-specific APIs are limited to setup, readiness, status, and teardown. Do not time a Herdr API shortcut against Uniterm's terminal path.
3. Every lifecycle trial uses a private HOME and XDG tree. Runtime directories must remain owner-only.
4. Network variability is removed from timing. Herdr's version and manifest checks remain disabled inside benchmark state, while their default behavior is assessed separately in the privacy rubric.
5. Terminal dimensions, shell, locale, environment, payload, settle time, and sampling windows are identical.
6. Startup order reverses across trials and full-suite order is seed-rotated.
7. Process metrics report both roots and transitive cohorts. Do not compare one contender's server PID with the other's full process tree.
8. Multi-pane sampling uses a fresh session so output scrollback and allocator retention do not bias memory.
9. Missing markers, invalid pane counts, readiness failures, premature exits, and incomplete core metrics are failures. Never convert them to zero, timeout values, or partial rankings.
10. Security/privacy scores remain separate from the performance index.
11. Persistence size, feature breadth, code size, and process count are context metrics, not performance-quality rankings.
12. Results within one percent remain ties in individual tables and in the balanced index; CPU metrics are floored at 0.1 percent of a core before ranking.
13. Readiness and control latency use the same semantic probe for both products (a fresh CLI listing the session's panes through the socket). `ut workspace list` exits 0 without a server and must not be used as a readiness signal.
14. Detached idle is sampled only after one attach/detach so both servers hold exactly one pane shell, and Herdr's headless grid is pinned to the profile geometry.

Latency markers intentionally clear/home the viewport, use octal shell escapes so input echo cannot satisfy observation, and change every visible cell between trials so full-frame and damage-based renderers are measured fairly.

## Benchmark and schema changes

When adding or changing a metric:

- Define its scenario, unit, direction, validity condition, and interpretation in `docs/METHODOLOGY.md`.
- Apply the same semantic workload to both adapters.
- Decide explicitly whether it is a core ranking metric or context only.
- Add tests for ranking behavior, failure behavior, and any parser or marker logic.
- Update report guidance and limitations.
- Bump `RESULT_SCHEMA_VERSION` for incompatible JSON changes.
- Keep report merge validation strict enough to prevent unlike profiles, contenders, or source revisions from being combined.

Do not silently change profile workloads. The current intent is:

| Profile | Use | Idle window | Startup / latency / output | Panes / lines |
| --- | --- | ---: | ---: | ---: |
| `smoke` | Adapter validation | 2 s | 2 / 5 / 2 | 4 / 1,000 |
| `standard` | Engineering comparison | 30 s | 8 / 30 / 5 | 8 / 10,000 |
| `marketing` | Public resource claims | 300 s | 20 / 100 / 10 | 16 / 50,000 |

Each profile also sets resize-storm iterations (smoke 5, standard 20, marketing 40) and extra multi-client attaches (2 for every profile). New scenarios must apply the same semantic workload to both adapters, be gated by the screen oracle where a final-screen assertion is meaningful, and be declared context rather than core unless the methodology is revised.

On Linux, CPU time is read from `/proc/<pid>/stat` (10 ms ticks) when available, so a 30 s window resolves about 0.03 percent of a core; elsewhere `ps` resolution applies and short runs commonly report zero idle CPU. Never use smoke or standard zeroes as evidence that a product consumes no CPU.

## Assurance rubric

Every assurance criterion must have exactly the same name and definition for every contender. Findings require evidence paths and one of `pass`, `partial`, `fail`, `unknown`, or `not_applicable`.

- Use source and official product documentation as evidence.
- Re-review evidence when either app revision changes.
- Prefer `unknown` over an unsupported favorable or unfavorable conclusion.
- Do not infer absence merely from one text search; inspect dependencies and relevant control flow.
- Never describe the checklist as a penetration test, certification, legal opinion, or privacy guarantee.
- Do not fold assurance scores into performance rankings.

Static lexical counts are implementation-surface context only. Keep exclusions for generated bindings, vendored code, translations, and test-only code explicit and symmetrical.

## Cross-platform requirements

- Avoid Linux-only `/proc` dependencies; process collection must continue to work with macOS `ps`.
- WSL follows the Linux execution path but must remain separately identified in reports.
- Keep PTY and process-group code within appropriate Unix configuration boundaries.
- Use short temporary paths because Unix socket paths have small platform limits.
- Do not assume GNU-only shell utilities in product workloads; `/bin/sh` commands must remain POSIX-compatible.
- A Linux test does not establish macOS or WSL performance. Public cross-platform claims require runs on those platforms.

## Development workflow

Build and validate with:

```sh
cargo fmt --all --check
cargo test --all-targets --locked
cargo clippy --all-targets --locked -- -D warnings
cargo build --release --locked
target/release/ut-compare doctor --config comparison.toml
```

For adapter, PTY, process sampling, or benchmark-orchestration changes, run a real-product smoke suite at minimum. Before replacing engineering results, run `standard`. Before making public idle-resource claims, run at least three `marketing` trials per required OS/host.

Real-product runs create Unix sockets and PTYs and may require sandbox approval. Always confirm that Uniterm and Herdr server/client processes have exited after failures as well as successful runs.

Use release binaries built from recorded source commits. Record and retain their SHA-256 hashes in raw reports. Do not present results from an artifact whose relationship to the stated source revision is uncertain. The reviewed application checkouts may carry unrelated uncommitted changes; build from clean clones of the committed revision under `.artifacts/` (see README) and point `comparison.local.toml` at those clones so the recorded revision is exactly what was built.

## Rust conventions

- The minimum Rust version is declared in `Cargo.toml`; do not raise it incidentally.
- Keep dependencies small and justified. This harness should remain easier to audit than either product it measures.
- Prefer explicit errors with benchmark/contender context over panics.
- Preserve atomic report writes.
- Keep unsafe PTY operations small, documented with safety comments, and covered by integration tests.
- Use deterministic ordered collections where output stability matters.
- Format generated human-readable values in reports, while retaining full raw samples in JSON.

## Generated and local files

Do not commit or depend on machine-specific contents under:

- `target/`
- `reports/`
- `.artifacts/`
- `comparison*.local.toml`

It is acceptable to create those locally for verification. Never hand-edit generated report numbers. Fix the harness or configuration and regenerate JSON and Markdown together. A Markdown report should be reproducible from its raw JSON using `ut-compare report`.

## Publication guidance

Every external comparison should include the raw JSON, readable report, tool revision, application revisions, exact artifact hashes, profile, host/kernel/hardware context, and operational notes. Phrase conclusions as applying to the measured workload and host.

Do not generalize a single Linux run to all Linux systems, WSL, macOS, or all user workflows. Correctness and workload validity gate performance: an application that drops required terminal state has not completed the test faster.

## Publication and output privacy

- The repository and package contain only the tool, neutral templates, methodology and synthetic tests. Do not add measured results, pre-scored product reviews, report bundles, source clones or release binaries.
- Keep MIT licensing and MAXART copyright intact.
- Commit messages must not contain AI co-author or AI session-link trailers.
- All CLI outputs and the primary runner/report APIs must pass through the output allowlist in `src/privacy.rs`. See `docs/PRIVACY.md`.
- Do not introduce a raw-output opt-out, include terminal dumps in errors, or enable retained workdirs. Sanitize imports of older schemas before generating output.
- Preserve numeric samples, workload validity, artifact hashes and source revisions; redact identity and arbitrary text rather than measurements.
- Validate the publication policy with `python3 scripts/check-publication.py` and inspect `cargo package --list --allow-dirty`.
