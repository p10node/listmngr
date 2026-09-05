"""Fixture-level regression tests for the production-crate acceptance gate."""

import contextlib
import io
import json
from pathlib import Path
import runpy
import subprocess
import tempfile
import unittest
from unittest.mock import patch


SCRIPT = Path(__file__).resolve().parents[1] / "check-production-crates.py"


class ProductionCratesTest(unittest.TestCase):
    def test_null_cargo_metadata_defaults_to_current_phase(self):
        with tempfile.TemporaryDirectory() as directory:
            source = Path(directory) / "lib.rs"
            package = {
                "name": "fixture",
                "metadata": None,
                "targets": [{"kind": ["lib"], "src_path": str(source)}],
            }
            result = subprocess.CompletedProcess(
                args=[], returncode=0, stdout=json.dumps({"packages": [package]})
            )
            for content, allowed in [
                ("pub fn implemented() {}\n", True),
                ("//! Documentation is not production code.\n", False),
            ]:
                with self.subTest(allowed=allowed):
                    source.write_text(content, encoding="utf-8")
                    with patch("subprocess.run", return_value=result):
                        with contextlib.redirect_stdout(io.StringIO()):
                            if allowed:
                                runpy.run_path(str(SCRIPT), run_name="__main__")
                            else:
                                with self.assertRaisesRegex(SystemExit, "empty production crate"):
                                    runpy.run_path(str(SCRIPT), run_name="__main__")


if __name__ == "__main__":
    unittest.main()
