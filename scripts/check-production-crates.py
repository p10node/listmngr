#!/usr/bin/env python3
"""Reject empty crates unless Cargo metadata explicitly defers their delivery phase."""

import json
import re
import subprocess
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
CURRENT_PHASE = 1


def production_text(source: Path) -> str:
    text = source.read_text(encoding="utf-8")
    text = re.sub(r"(?m)^\s*#!\[[^\n]+\]\s*$", "", text)
    text = re.sub(r"(?m)^\s*//[/!]?.*$", "", text)
    text = re.sub(r"/\*.*?\*/", "", text, flags=re.DOTALL)
    return text.strip()


metadata = json.loads(
    subprocess.run(
        ["cargo", "metadata", "--locked", "--no-deps", "--format-version", "1"],
        cwd=ROOT,
        check=True,
        capture_output=True,
        text=True,
    ).stdout
)

deferred = []
for package in metadata["packages"]:
    phase = (package.get("metadata") or {}).get("listmngr", {}).get("delivery-phase", CURRENT_PHASE)
    library_targets = [target for target in package["targets"] if "lib" in target["kind"]]
    for target in library_targets:
        source = Path(target["src_path"])
        if production_text(source):
            continue
        if phase <= CURRENT_PHASE:
            raise SystemExit(
                f"anti-stub: {package['name']} is an empty production crate claimed by phase {phase}"
            )
        deferred.append(f"{package['name']} (phase {phase})")

print("anti-stub: implemented current-phase production crates are non-empty")
if deferred:
    print("anti-stub: detected and explicitly deferred empty crates: " + ", ".join(deferred))