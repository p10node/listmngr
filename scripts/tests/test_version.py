"""The release version (P7-1.0): one version across the workspace, the chart
and the changelog, and no sentence left saying it is unreleased."""

import re
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
VERSION = "1.1.0"


def read(path: str) -> str:
    return (ROOT / path).read_text(encoding="utf-8")


class VersionTest(unittest.TestCase):
    def test_every_workspace_package_and_path_dependency_carries_the_version(self):
        self.assertIn(f'version = "{VERSION}"', read("Cargo.toml"))
        for manifest in sorted((ROOT / "crates").glob("*/Cargo.toml")):
            with self.subTest(manifest=manifest.name):
                text = manifest.read_text(encoding="utf-8")
                self.assertIn("version.workspace = true", text)
                for dep_version in re.findall(r'listmngr-\w+ = \{ path = "[^"]+", version = "([^"]+)" \}', text):
                    self.assertEqual(dep_version, VERSION)
        lock = read("Cargo.lock")
        self.assertIn(f'name = "listmngr"\nversion = "{VERSION}"', lock)
        self.assertNotIn('name = "listmngr"\nversion = "0.1.0"', lock)

    def test_the_chart_and_the_changelog_carry_the_version(self):
        chart = read("deploy/helm/listmngr/Chart.yaml")
        self.assertIn(f"version: {VERSION}", chart)
        self.assertIn(f'appVersion: "{VERSION}"', chart)
        changelog = read("CHANGELOG.md")
        self.assertIsNotNone(re.search(rf"^## \[{re.escape(VERSION)}\] - \d{{4}}-\d{{2}}-\d{{2}}$", changelog, re.M))
        self.assertLess(changelog.index("## [Unreleased]"), changelog.index(f"## [{VERSION}]"))

    def test_no_sentence_still_calls_the_version_unreleased(self):
        for path, needle in [
            ("CLAUDE.md", "means unreleased"),
            ("docs/SECURITY.md", "No public release is currently declared"),
            ("docs/ACCEPTANCE_NOTES.md", "0.1.0 (unreleased development)"),
            ("docs/ACCEPTANCE_NOTES.md", "no released tag is implied"),
            ("docs/book/src/README.md", "pre-release of the first version"),
        ]:
            with self.subTest(path=path, needle=needle):
                self.assertNotIn(needle, read(path))

    def test_the_api_document_and_the_cli_report_the_package_version(self):
        self.assertNotIn('version = "0.1.0"', read("crates/api/src/lib.rs"))
        self.assertIn('env!("CARGO_PKG_VERSION")', read("crates/cli/tests/cli.rs"))


if __name__ == "__main__":
    unittest.main()
