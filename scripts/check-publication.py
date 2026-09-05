#!/usr/bin/env python3
"""Check the tool-only publication boundary without printing sensitive content."""
import fnmatch
import re
import subprocess
from pathlib import Path
import tomllib


def git(*args):
    return subprocess.check_output(["git", *args], text=True)


def forbidden(path):
    return (
        path.startswith(("reports/", ".artifacts/", "target/", "docs/benchmarks/", "public/benchmarks/"))
        or path in {"docs/APP_REVIEW.md", "comparison.resize.toml", "credentials.json", "secrets.json"}
        or fnmatch.fnmatch(path, "comparison*.local.toml")
        or (Path(path).name.startswith(".env") and Path(path).name not in {".env.example", ".env.sample"})
        or path.endswith((".pem", ".key"))
    )


errors = []
files = set(git("ls-files", "--cached", "--others", "--exclude-standard").splitlines())
for path in files:
    if Path(path).is_file() and forbidden(path):
        errors.append("A local/evidence/secret file is in the publication tree.")

history_paths = set(git("log", "--all", "--format=", "--name-only").splitlines())
if any(forbidden(path) for path in history_paths if path):
    errors.append("Local or evidence files remain in Git history.")

messages = git("log", "--all", "--format=%B")
ai = r"(?:claude|codex|chatgpt|openai|anthropic|copilot|gemini|cursor)"
if re.search(r"(?im)^co-authored-by:.*" + ai, messages) or re.search(r"(?im)^(?:" + ai + r")[\w-]*-session(?:-id)?:", messages):
    errors.append("AI attribution or session trailers remain in commit messages.")

for name in ("comparison.toml", "tmux.contender.toml"):
    data = tomllib.loads(Path(name).read_text())
    for contender in data["contenders"]:
        if any(a["status"] != "unknown" or a.get("evidence") for a in contender.get("assurance", [])):
            errors.append("A distributed configuration contains application findings.")
        if any(not contender[key].startswith(".artifacts/") for key in ("binary", "source")):
            errors.append("A distributed configuration contains nonportable local paths.")

license_text = Path("LICENSE").read_text()
if "MIT License" not in license_text or "Copyright (c) 2026 MAXART" not in license_text:
    errors.append("The MAXART MIT license is missing or inconsistent.")

if errors:
    for error in dict.fromkeys(errors):
        print(error)
    raise SystemExit(1)
print("Publication policy passed: tool-only tree/history, neutral configs, license, and commit metadata.")
