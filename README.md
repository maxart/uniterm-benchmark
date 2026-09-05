# ut-compare

A reproducible terminal multiplexer benchmark for Uniterm (`ut`), Herdr, and optional tmux on native Linux, WSL-as-Linux, and macOS.

This repository contains the tool, methodology, neutral configuration templates, and synthetic tests. It does not distribute benchmark results, product source clones, binaries, or pre-scored application reviews.

## What it measures

All contenders receive equivalent shell workloads through real attached PTYs. The suite measures startup and control latency, detached and attached CPU/RSS, input visibility, output completion, pane scaling and recovery, resize storms, multiple clients, shutdown, restart, and artifact/state footprint. A screen oracle rejects incorrect or incomplete final output.

The balanced index uses eight shared core metrics. Missing or failed core measurements suppress ranking. CPU metrics use a 0.1-percent-of-one-core floor; values within one percent are ties. Feature breadth, persistence, static counts, and assurance ratings do not enter the performance index. Unsupported feature context is N/A, never a performance penalty.

- [Methodology and metric definitions](docs/METHODOLOGY.md)
- [Output privacy contract](docs/PRIVACY.md)
- [Optional assurance rubric](docs/ASSURANCE.md)
- [Contributor instructions](AGENTS.md)

## Build

Requires Rust 1.85 or later, `git`, `ps`, `uname`, and `/bin/sh`. Product builds have their own toolchain requirements.

```sh
cargo build --release --locked
```

Build the selected product revisions from clean source clones, keep their dependencies locked, and retain artifact hashes. Do not identify an arbitrary installed binary with a source checkout unless you can establish that relationship. The tool records source commits and hashes, but cannot prove how an externally supplied binary was built.

Default template layout:

```text
.artifacts/
  uniterm-src/                  # clean source checkout
  uniterm-target/release/ut
  herdr-src/                    # clean source checkout
  herdr-target/release/herdr
  tmux-src/                     # optional clean source checkout
  tmux-target/release/tmux
```

These directories are local and ignored. Use the products' build instructions for your chosen revisions. Herdr builds may require a specific Zig toolchain for the vendored terminal engine.

## Configure and run

Copy the neutral template, then set the local artifact/source paths. Relative paths resolve beside the config file, so keep the local config in the repository root.

```sh
cp comparison.toml comparison.local.toml
# Edit comparison.local.toml to point to your clean builds.
target/release/ut-compare doctor --config comparison.local.toml
target/release/ut-compare run --config comparison.local.toml --profile smoke \
  --output reports/smoke.json --markdown reports/smoke.md
```

Profiles have fixed workloads:

| Profile | Purpose | Idle window | Startup / latency / output samples | Panes / output lines |
| --- | --- | ---: | ---: | ---: |
| `smoke` | Adapter validation | 2 s | 2 / 5 / 2 | 4 / 1,000 |
| `standard` | Engineering comparison | 30 s | 8 / 30 / 5 | 8 / 10,000 |
| `marketing` | Longer resource measurement | 300 s | 20 / 100 / 10 | 16 / 50,000 |

Use standard or marketing only after smoke succeeds. Short-run CPU zeroes do not establish zero CPU consumption. A run with measurement failures still writes its sanitized evidence, then exits unsuccessfully.

```sh
target/release/ut-compare run --config comparison.local.toml --profile standard \
  --seed 1 --output reports/standard.json --markdown reports/standard.md
target/release/ut-compare report --output reports/combined.md reports/run-1.json reports/run-2.json
```

Merge validation rejects incompatible schemas, profiles, contenders, adapters, and known source revisions. Keep operating conditions comparable and use different seeds to rotate contender order. Linux measurements do not establish WSL or macOS behavior. This repository publishes no comparison evidence; any measurements you produce remain your own local outputs.

## Add tmux

Append the optional neutral contender once, then edit its paths if necessary:

```sh
cat comparison.local.toml tmux.contender.toml > comparison.tmux.local.toml
target/release/ut-compare doctor --config comparison.tmux.local.toml
target/release/ut-compare run --config comparison.tmux.local.toml --profile smoke \
  --output reports/tmux-smoke.json --markdown reports/tmux-smoke.md
```

The tmux adapter uses a private socket and explicit config, a non-login `/bin/sh`, the common terminal type, and tiled prefix-key splits. Status, history, and rendering timers keep product defaults. Existing user sessions are outside the benchmark. Native disk restoration and Rust/Cargo-specific counts are N/A; fresh-session restart readiness and disk bytes remain measured context. See the [adapter methodology](docs/METHODOLOGY.md#tmux-adapter).

## Sanitized output

Schema 7 output omits hostnames, paths, wall-clock timestamps, user-defined names/titles, raw terminal/error output, and free-text review content. It preserves measurements, statuses, core counts, numeric version information, source revisions, and artifact hashes. No opt-out exports raw diagnostics or keeps product work directories. Command-line progress and errors also omit identifying details.

Schema 5/6 input can be converted without changing its numeric measurements:

```sh
target/release/ut-compare sanitize old.json --output reports/sanitized.json \
  --markdown reports/sanitized.md
```

`report` also filters imported reports before rendering. The compatibility `--title` option is ignored; report titles are fixed. Sanitization does not make hardware characteristics or program hashes anonymous, and it does not sanitize files created independently by your shell or other programs. See [the exact contract](docs/PRIVACY.md).

## Development checks

```sh
cargo fmt --all --check
cargo test --all-targets --locked
cargo clippy --all-targets --locked -- -D warnings
cargo build --release --locked
python3 scripts/check-publication.py
python3 scripts/check-package.py
```

Real-product regression tests are explicit and use isolated sockets/PTYs:

```sh
UT_COMPARE_TMUX_BINARY=/path/to/your/tmux \
  cargo test --locked --test tmux -- --ignored --test-threads=1
```

Adapter or orchestration changes also require a real-product smoke run. CI checks the Rust code, secret scanning, repository publication policy, and package contents; it does not publish measurements or upload build/report artifacts.

## License

[MIT](LICENSE), copyright (c) 2026 MAXART.
