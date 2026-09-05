"""Keep locally verified acceptance probes blocking in CI as well."""

from pathlib import Path
import unittest


ROOT = Path(__file__).resolve().parents[2]


class AcceptanceWiringTest(unittest.TestCase):
    def test_required_phase_one_probes_are_not_compile_only(self):
        workflow = (ROOT / ".github/workflows/ci.yml").read_text(encoding="utf-8")
        postgres = (ROOT / "scripts/test-postgres.sh").read_text(encoding="utf-8")
        for command in [
            "python3 -m unittest discover -s scripts/tests",
            "python3 scripts/check-production-crates.py",
            "python3 scripts/test-mailmanclient.py",
            "scripts/test-postgres.sh",
            "systemd-analyze verify deploy/systemd/listmngr.service",
        ]:
            with self.subTest(command=command):
                self.assertIn(command, workflow)
        self.assertIn("live_postgres_matches_the_exact_sqlite_semantic_corpus", postgres)
        self.assertIn("postgres_scoped_user_routes_allow_inside_and_deny_outside_bounds", postgres)
        self.assertIn("--ignored --exact", postgres)


if __name__ == "__main__":
    unittest.main()
