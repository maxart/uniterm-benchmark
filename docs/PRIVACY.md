# Output privacy

The CLI's `run`, `report`, `sanitize`, and `doctor` commands produce sanitized output by default. There is no raw-output switch. The main runner and Markdown renderer apply the same filter for library callers. Lower-level PTY/adaptor APIs necessarily observe raw data in memory; callers using those APIs must not serialize it themselves.

## Retained data

- Numeric workload settings, raw numeric samples, summaries, units, directions, and measurement statuses.
- OS family, architecture, logical CPU count, numeric kernel/compiler versions, CPU sampling source, and WSL status.
- Source commit IDs, source dirtiness, supported package license identifiers, executable byte counts, numeric version information, and artifact SHA-256 hashes.
- Allowlisted numeric scenario metadata, including pane counts, geometry, RSS curves, retry counts, and yes/no/unknown restoration observations.
- Standard assurance criterion names, categories, operator-supplied statuses and weights. These ratings are not independently verified by the harness.

These fields are useful for comparison and reproducibility. They can still identify software builds or distinguish hardware. Sanitized is not synonymous with anonymous. A malicious input can also encode data into otherwise valid numbers or hashes; this filter is not a general data-loss-prevention system.

## Omitted data

- Machine hostname: replaced with `host`.
- Local executable/source paths: replaced with generic contender aliases.
- Operator-supplied contender IDs and display names: replaced consistently with adapter aliases.
- Exact run timestamps and source commit-date strings. Run IDs become opaque SHA-256 identifiers; they are pseudonyms, not anonymization guarantees.
- Custom report titles, arbitrary notes, review rationale/evidence paths, extra metadata keys, and unrecognized package-license strings.
- Product stdout/stderr diagnostics, terminal screen dumps, command strings, arbitrary configuration values and environment contents.
- Raw errors and warning descriptions. Counts and failure statuses remain visible; the tool emits fixed diagnostic messages instead.

The allowlist replaces free-text fields instead of trying to recognize every possible secret. It never changes numeric samples or converts failure to zero or N/A. Unknown adapter/metric/unit/profile formats fail closed rather than being echoed. Supported numeric version information is retained; build-specific suffix text becomes `-redacted`, so a prerelease is not labeled as a stable release. tmux's single-letter release suffix is retained. Exact source revisions and hashes remain the artifact identity.

Output files are replaced atomically using exclusively created temporary files with owner-only permissions on Unix. Failed writes remove their temporary files.

`keep_workdirs = true` is rejected. Product work directories are created exclusively with owner-only permissions and removed after execution; the harness does not retain a server log. Forced termination or an OS failure can prevent cleanup; any remaining work directory still has owner-only permissions. Files and output written independently by your shell, editor, build tools, or other programs are outside this contract. Existing ignored local data is not silently modified.

## Older reports

Schema 7 introduces the output privacy contract and omits serialized wall-clock timestamps. Schema 5 and 6 reports can be read, but their identities and free text are filtered before rendering or exporting. Validation of the original schemas, contender sets, profiles and source revisions happens before sanitization so redaction cannot make incompatible inputs mergeable.

```sh
ut-compare sanitize old.json --output sanitized.json --markdown sanitized.md
```

The original input remains unchanged unless you explicitly choose the same output path. New Markdown always uses the fixed title `Terminal multiplexer comparison`; the older `--title` option is accepted for compatibility but ignored.

Use `doctor` to verify setup before running measurements. CLI errors deliberately omit private diagnostics; a failed run still writes its sanitized result when possible and exits nonzero. Test fixtures exercise path/configuration failures and malicious strings without using real credentials.
