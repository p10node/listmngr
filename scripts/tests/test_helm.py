"""Helm as a real install (P10-HELM-TEST): the chart carries a test hook, a
values schema and its own service account, lints strictly and renders every
documented variant, and a disposable kind cluster installs it — in CI through
the `helm` job and locally through scripts/test-helm.sh."""

import json
import re
import shutil
import stat
import subprocess
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
CHART = ROOT / "deploy/helm/listmngr"

# Every variant the chart documents renders; the harness installs the first.
PG = ["--set", "postgresql.auth.password=kind-only-password"]
SQLITE = ["--set", "postgresql.enabled=false", "--set-string",
          "secrets.LISTMNGR__DATABASE__URL=sqlite:///var/lib/listmngr/listmngr.db?mode=rwc"]
VARIANTS = {
    "default": PG,
    "secret-files-and-ingress": PG + ["--set", "secretFiles.dkim\\.pem=KEY", "--set", "ingress.enabled=true"],
    "existing-secrets": PG + ["--set", "existingSecret=lists-env", "--set", "existingFilesSecret=lists-files"],
    "sqlite-no-persistence-no-service-account": SQLITE + ["--set", "persistence.enabled=false", "--set", "serviceAccount.create=false"],
    "network-policy": PG + ["--set", "networkPolicy.enabled=true"],
}


def read(path: str) -> str:
    return (ROOT / path).read_text(encoding="utf-8")


def helm(*args: str) -> subprocess.CompletedProcess:
    return subprocess.run(["helm", *args], cwd=ROOT, capture_output=True, text=True)


class HelmTest(unittest.TestCase):
    def test_the_chart_carries_a_test_hook_a_schema_and_a_service_account(self):
        hook = (CHART / "templates/tests/test-ready.yaml").read_text(encoding="utf-8")
        for needle in ['"helm.sh/hook": test', "wget", "/readyz", "restartPolicy: Never", "automountServiceAccountToken: false"]:
            with self.subTest(needle=needle):
                self.assertIn(needle, hook)
        schema = json.loads((CHART / "values.schema.json").read_text(encoding="utf-8"))
        self.assertIs(schema["additionalProperties"], False, "an unknown top-level value is a typo, not a setting")
        values = (CHART / "values.yaml").read_text(encoding="utf-8")
        for key in re.findall(r"^([A-Za-z]\w*):", values, re.M):
            with self.subTest(key=key):
                self.assertIn(key, schema["properties"])
        account = (CHART / "templates/serviceaccount.yaml").read_text(encoding="utf-8")
        self.assertIn("kind: ServiceAccount", account)
        self.assertIn("automountServiceAccountToken: false", account)
        deployment = (CHART / "templates/deployment.yaml").read_text(encoding="utf-8")
        self.assertIn("serviceAccountName:", deployment)
        self.assertIn("automountServiceAccountToken: false", deployment)
        chart = (CHART / "Chart.yaml").read_text(encoding="utf-8")
        self.assertIn("keywords:", chart)
        self.assertIn("maintainers:", chart)

    def test_the_chart_lints_strictly_and_renders_every_variant(self):
        if shutil.which("helm") is None:
            self.skipTest("helm is not installed here; CI's helm job lints")
        lint = helm("lint", "--strict", str(CHART), *PG)
        self.assertEqual(lint.returncode, 0, lint.stdout + lint.stderr)
        for name, extra in VARIANTS.items():
            with self.subTest(variant=name):
                rendered = helm("template", "t", str(CHART), *extra)
                self.assertEqual(rendered.returncode, 0, rendered.stdout + rendered.stderr)
                self.assertIn("kind: Deployment", rendered.stdout)
                if "serviceAccount.create=false" in extra:
                    self.assertNotIn("kind: ServiceAccount", rendered.stdout)
                    self.assertNotIn("kind: PersistentVolumeClaim", rendered.stdout)
                    self.assertNotIn("kind: StatefulSet", rendered.stdout)
                    self.assertIn("emptyDir: {}", rendered.stdout)
                else:
                    self.assertIn("kind: ServiceAccount", rendered.stdout)
                    self.assertIn("kind: PersistentVolumeClaim", rendered.stdout)
                    self.assertIn("kind: StatefulSet", rendered.stdout)
                if "networkPolicy.enabled=true" in extra:
                    self.assertEqual(rendered.stdout.count("kind: NetworkPolicy"), 2)
                else:
                    self.assertNotIn("kind: NetworkPolicy", rendered.stdout)
        rejected = helm("template", "t", str(CHART), *PG, "--set", "replicas=2")
        self.assertNotEqual(rejected.returncode, 0, "a misspelled value must not render silently")
        self.assertIn("replicas", rejected.stderr)

    def test_postgresql_is_in_the_chart_and_the_url_is_assembled(self):
        if shutil.which("helm") is None:
            self.skipTest("helm is not installed here; CI's helm job lints")
        rendered = helm("template", "t", str(CHART), *PG)
        self.assertEqual(rendered.returncode, 0, rendered.stderr)
        out = rendered.stdout
        for needle in [
            "kind: StatefulSet", "name: t-listmngr-postgresql", "clusterIP: None",
            "postgres:17-alpine@sha256:18cfe3ef5e6815560c98237d6216d1e5119702fb0f3894c8785dd58b8bbe5d73",
            "pg_isready", "PGDATA", "/var/lib/postgresql/data/pgdata", "runAsUser: 70", "fsGroup: 70",
            "name: wait-db", "value: postgres://listmngr:$(POSTGRES_PASSWORD)@t-listmngr-postgresql:5432/listmngr",
            "checksum/secrets:",
        ]:
            with self.subTest(needle=needle):
                self.assertIn(needle, out)
        self.assertNotIn("change-me", out, "no placeholder URL is rendered any more")
        # The password is read at the database's first start; the application reads it as the URL.
        self.assertIn("key: password", out)
        # Without a password the install fails early, like Compose's POSTGRES_PASSWORD:?.
        missing = helm("template", "t", str(CHART))
        self.assertNotEqual(missing.returncode, 0)
        self.assertIn("postgresql.auth.password", missing.stderr)
        # A password that would need URL-escaping is refused by the schema.
        unsafe = helm("template", "t", str(CHART), "--set", "postgresql.auth.password=has/slash@and:colon")
        self.assertNotEqual(unsafe.returncode, 0)
        # An existing Secret holds the password instead; no chart Secret for it then.
        existing = helm("template", "t", str(CHART), "--set", "postgresql.auth.existingSecret=pg-secret")
        self.assertEqual(existing.returncode, 0, existing.stderr)
        self.assertIn("name: pg-secret", existing.stdout)
        self.assertNotIn("name: t-listmngr-postgresql\n  labels:\n    helm.sh/chart: \"listmngr-1.1.0\"\n    app.kubernetes.io/name: listmngr\n    app.kubernetes.io/instance: t\n    app.kubernetes.io/version: \"1.1.0\"\n    app.kubernetes.io/managed-by: Helm\ntype: Opaque", existing.stdout)
        # Without PostgreSQL, the operator must say where the database is.
        nowhere = helm("template", "t", str(CHART), "--set", "postgresql.enabled=false")
        self.assertNotEqual(nowhere.returncode, 0)
        self.assertIn("LISTMNGR__DATABASE__URL", nowhere.stderr)
        sqlite = helm("template", "t", str(CHART), *SQLITE)
        self.assertEqual(sqlite.returncode, 0, sqlite.stderr)
        self.assertNotIn("kind: StatefulSet", sqlite.stdout)
        self.assertNotIn("name: wait-db", sqlite.stdout)

    def test_the_network_policy_fences_the_database_and_the_web(self):
        if shutil.which("helm") is None:
            self.skipTest("helm is not installed here; CI's helm job lints")
        rendered = helm("template", "t", str(CHART), *PG, "--set", "networkPolicy.enabled=true")
        self.assertEqual(rendered.returncode, 0, rendered.stderr)
        out = rendered.stdout
        self.assertEqual(out.count("kind: NetworkPolicy"), 2)
        for needle in ["app.kubernetes.io/component: postgresql", "port: 5432", "port: 8000", "port: 53", "policyTypes:"]:
            with self.subTest(needle=needle):
                self.assertIn(needle, out)

    def test_the_kind_harness_exists_and_ci_runs_it(self):
        script = ROOT / "scripts/test-helm.sh"
        self.assertTrue(script.stat().st_mode & stat.S_IXUSR, "the harness is executable")
        text = script.read_text(encoding="utf-8")
        for needle in [
            "kind create cluster", "kindest/node:", "@sha256:", "kind load docker-image",
            "helm install", "--wait", "helm test", "user create", "/readyz", "listmngr status",
            "helm upgrade", "checksum", "helm uninstall", "kind delete cluster",
            "--sqlite", "postgresql.auth.password", "pg_isready", "postgres:17-alpine@sha256:",
        ]:
            with self.subTest(needle=needle):
                self.assertIn(needle, text)
        workflow = read(".github/workflows/ci.yml")
        self.assertIn("scripts/test-helm.sh", workflow)
        self.assertIn("helm lint --strict deploy/helm/listmngr", workflow)
        self.assertRegex(workflow, r"uses: helm/kind-action@[0-9a-f]{40} # v\d", "kind-action pinned by commit")

    def test_the_deployment_readme_documents_helm(self):
        readme = read("deploy/README.md")
        self.assertIn("## Helm", readme)
        self.assertIn("scripts/test-helm.sh", readme)
        self.assertIn("helm test", readme)


if __name__ == "__main__":
    unittest.main()
