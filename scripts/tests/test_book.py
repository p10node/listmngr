"""The operator's book (P7-DOCS-SITE): the generated configuration reference
matches the source, every link and include in the book resolves, and CI
builds it."""

import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
BOOK = ROOT / "docs/book"


def run(*args: str) -> subprocess.CompletedProcess:
    return subprocess.run([sys.executable, *args], cwd=ROOT, capture_output=True, text=True)


class BookTest(unittest.TestCase):
    def test_the_book_has_its_chapters(self):
        summary = (BOOK / "src/SUMMARY.md").read_text(encoding="utf-8")
        for chapter in [
            "install.md", "configuration.md", "configuration-reference.md", "mail.md",
            "migration.md", "operations.md", "admin.md", "api.md", "security.md",
            "architecture.md", "changelog.md",
        ]:
            with self.subTest(chapter=chapter):
                self.assertIn(f"({chapter})", summary)
                self.assertTrue((BOOK / "src" / chapter).exists(), chapter)
        self.assertIn('build-dir = "book"', (BOOK / "book.toml").read_text(encoding="utf-8"))

    def test_the_configuration_reference_is_generated_from_the_source(self):
        check = run("scripts/config-reference.py", "--check")
        self.assertEqual(check.returncode, 0, check.stdout + check.stderr)
        with tempfile.TemporaryDirectory() as tmp:
            stale = Path(tmp) / "configuration-reference.md"
            stale.write_text("# Configuration reference\n\nstale\n", encoding="utf-8")
            result = run("scripts/config-reference.py", "--check", "--output", str(stale))
            self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
            self.assertIn("out of date", result.stderr)

    def test_every_link_and_include_in_the_book_resolves(self):
        check = run("scripts/check-book-links.py")
        self.assertEqual(check.returncode, 0, check.stdout + check.stderr)
        with tempfile.TemporaryDirectory() as tmp:
            src = Path(tmp) / "src"
            src.mkdir()
            (src / "SUMMARY.md").write_text("# Summary\n\n- [Here](here.md)\n- [Gone](gone.md)\n", encoding="utf-8")
            (src / "here.md").write_text("[dangling](nowhere.md)\n\n{{#include ../missing.md}}\n", encoding="utf-8")
            result = run("scripts/check-book-links.py", "--src", str(src))
            self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
            for problem in ["gone.md", "nowhere.md", "missing.md"]:
                with self.subTest(problem=problem):
                    self.assertIn(problem, result.stderr)

    def test_ci_builds_the_book(self):
        workflow = (ROOT / ".github/workflows/ci.yml").read_text(encoding="utf-8")
        for command in [
            "python3 scripts/config-reference.py --check",
            "python3 scripts/check-book-links.py",
            "mdbook build docs/book",
            "mdbook@0.5.4",
        ]:
            with self.subTest(command=command):
                self.assertIn(command, workflow)


if __name__ == "__main__":
    unittest.main()
