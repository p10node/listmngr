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
VARIANTS = {
    "default": [],
    "secret-files-and-ingress": ["--set", "secretFiles.dkim\\.pem=KEY", "--set", "ingress.enabled=true"],
    "existing-secrets": ["--set", "existingSecret=lists-env", "--set", "existingFilesSecret=lists-files"],
    "no-persistence-no-service-account": ["--set", "persistence.enabled=false", "--set", "serviceAccount.create=false"],
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
        lint = helm("lint", "--strict", str(CHART))
        self.assertEqual(lint.returncode, 0, lint.stdout + lint.stderr)
        for name, extra in VARIANTS.items():
            with self.subTest(variant=name):
                rendered = helm("template", "t", str(CHART), *extra)
                self.assertEqual(rendered.returncode, 0, rendered.stdout + rendered.stderr)
                self.assertIn("kind: Deployment", rendered.stdout)
                if "serviceAccount.create=false" in extra:
                    self.assertNotIn("kind: ServiceAccount", rendered.stdout)
                    self.assertNotIn("kind: PersistentVolumeClaim", rendered.stdout)
                    self.assertIn("emptyDir: {}", rendered.stdout)
                else:
                    self.assertIn("kind: ServiceAccount", rendered.stdout)
                    self.assertIn("kind: PersistentVolumeClaim", rendered.stdout)
        rejected = helm("template", "t", str(CHART), "--set", "replicas=2")
        self.assertNotEqual(rejected.returncode, 0, "a misspelled value must not render silently")
        self.assertIn("replicas", rejected.stderr)

    def test_the_kind_harness_exists_and_ci_runs_it(self):
        script = ROOT / "scripts/test-helm.sh"
        self.assertTrue(script.stat().st_mode & stat.S_IXUSR, "the harness is executable")
        text = script.read_text(encoding="utf-8")
        for needle in [
            "kind create cluster", "kindest/node:", "@sha256:", "kind load docker-image",
            "helm install", "--wait", "helm test", "user create", "/readyz", "listmngr status",
            "helm upgrade", "checksum", "helm uninstall", "kind delete cluster",
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
