#!/usr/bin/env python3
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
CRATES = ["core", "db", "mail", "pipeline", "runners", "archive", "api", "web", "cli"]
required = [ROOT / "Cargo.toml", ROOT / "rust-toolchain.toml", ROOT / "deny.toml", ROOT / "rustfmt.toml", ROOT / "clippy.toml"]
required += [ROOT / "crates" / crate / "Cargo.toml" for crate in CRATES]
required += [
    ROOT / "README.md", ROOT / "CLAUDE.md", ROOT / "CHANGELOG.md", ROOT / "LICENSE",
    ROOT / "docs" / "ARCHITECTURE.md", ROOT / "docs" / "FEATURE_PARITY.md",
    ROOT / "docs" / "SECURITY.md", ROOT / "docs" / "adr" / "0001-db-queue.md",
    ROOT / "docs" / "adr" / "0002-ssr-htmx.md", ROOT / ".github" / "workflows" / "ci.yml",
    ROOT / "deploy" / "Dockerfile", ROOT / "deploy" / "docker-compose.yml", ROOT / ".env.example",
]
missing = [str(path.relative_to(ROOT)) for path in required if not path.exists()]
assert not missing, f"missing workspace bootstrap files: {missing}"
print("workspace contract: ok")
