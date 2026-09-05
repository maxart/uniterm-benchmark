#!/usr/bin/env python3
"""Cargo include patterns must not pull ignored files into a source package."""
import subprocess

files = subprocess.check_output(
    ["cargo", "package", "--list", "--locked", "--allow-dirty"], text=True
).splitlines()
allowed = {
    "Cargo.toml", "Cargo.toml.orig", "Cargo.lock", "LICENSE", "README.md",
    ".cargo_vcs_info.json", "comparison.toml", "tmux.contender.toml",
    "docs/METHODOLOGY.md", "docs/ASSURANCE.md", "docs/PRIVACY.md",
}
for path in files:
    if path in allowed or (path.startswith(("src/", "tests/")) and path.endswith(".rs")):
        continue
    raise SystemExit("Unexpected content in source package; review anchored include patterns.")
if not {"LICENSE", "src/privacy.rs", "tests/privacy.rs", "comparison.toml"}.issubset(files):
    raise SystemExit("Required source, license, or privacy checks are absent from the package.")
print(f"Package policy passed: {len(files)} tool/license/documentation/test files.")
