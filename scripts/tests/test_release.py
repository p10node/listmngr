"""The release pipeline (P7-RELEASE): a tag builds every platform from pinned
actions, the packages carry the unit and the service account, the chart is
hardened and lints, CI lints it on every push, and the upgrade policy is
written."""

import re
import shutil
import subprocess
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
CHART = ROOT / "deploy/helm/listmngr"


def read(path: str) -> str:
    return (ROOT / path).read_text(encoding="utf-8")


class ReleaseTest(unittest.TestCase):
    def test_a_tag_builds_every_platform_from_pinned_actions(self):
        workflow = read(".github/workflows/release.yml")
        self.assertIn('tags: ["v*"]', workflow)
        self.assertIsNone(re.search(r"uses: [^#\s]+@v\d", workflow), "every action pinned by commit")
        for needle in [
            "x86_64-unknown-linux-musl", "aarch64-unknown-linux-musl",
            "aarch64-apple-darwin", "x86_64-apple-darwin",
            "cargo build --locked --release -p listmngr --target",
            "cargo deb", "cargo generate-rpm", "cargo cyclonedx",
            "file: deploy/Dockerfile",
            "file: deploy/postfix/Dockerfile", "ghcr.io/${{ github.repository }}-postfix",
            "cosign sign --yes", "cosign sign-blob --yes", "SHA256SUMS",
            "helm registry login ghcr.io", "helm push", "oci://ghcr.io/${{ github.repository_owner }}/charts",
            'cosign sign --yes "ghcr.io/${{ github.repository_owner }}/charts/listmngr@',
            "helm lint --strict deploy/helm/listmngr", "helm package deploy/helm/listmngr",
        ]:
            with self.subTest(needle=needle):
                self.assertIn(needle, workflow)

    def test_images_are_built_natively_per_architecture_and_joined_into_one_manifest(self):
        workflow = read(".github/workflows/release.yml")
        image = workflow[workflow.index("  image:\n"):workflow.index("  image-manifest:\n")]
        manifest = workflow[workflow.index("  image-manifest:\n"):workflow.index("  chart:\n")]
        # One native runner per architecture, each pushing by digest.
        for needle in ["platform: linux/amd64", "runner: ubuntu-24.04", "platform: linux/arm64", "runner: ubuntu-24.04-arm",
                       "runs-on: ${{ matrix.runner }}", "push-by-digest=true", "name-canonical=true", "upload-artifact@"]:
            with self.subTest(needle=needle):
                self.assertIn(needle, image)
        self.assertNotIn("setup-qemu-action", workflow, "no emulation: the arm64 build took 138 minutes under QEMU")
        self.assertNotIn("platforms: linux/amd64,linux/arm64", workflow)
        # The manifest job joins the digests under the version tags and signs the lists.
        for needle in ["needs: image", "download-artifact@", "docker buildx imagetools create", "imagetools inspect",
                       "cosign sign --yes", "-postfix@"]:
            with self.subTest(needle=needle):
                self.assertIn(needle, manifest)
        self.assertIn("needs: [binaries, packages, sbom, image-manifest, chart]", workflow)
        # The build steps upload no `.dockerbuild` record artifacts, and publish
        # downloads only the artifacts it names: the first release run's publish
        # failed in download-artifact with two such records among the artifacts.
        self.assertIn('DOCKER_BUILD_RECORD_UPLOAD: "false"', image)
        publish = workflow[workflow.index("  publish:\n"):]
        self.assertIn('pattern: "{binary-*,packages,sbom,chart}"', publish)
        self.assertIn("needs.image-manifest.outputs.digest", workflow)
        self.assertIn("needs.image-manifest.outputs.postfix_digest", workflow)

    def test_the_chart_job_logs_in_to_the_registry_before_cosign_signs(self):
        workflow = read(".github/workflows/release.yml")
        chart = workflow[workflow.index("  chart:\n"):workflow.index("  publish:\n")]
        # cosign reads Docker's credential store, not Helm's registry config:
        # without this login the signature push was refused on the first run.
        self.assertLess(chart.index("docker/login-action@"), chart.index("cosign sign --yes"))
        self.assertIn("registry: ghcr.io", chart)

    def test_the_packages_carry_the_unit_the_account_and_the_directories(self):
        manifest = read("crates/cli/Cargo.toml")
        self.assertIn("[package.metadata.deb]", manifest)
        self.assertIn("[package.metadata.generate-rpm]", manifest)
        self.assertIn("listmngr.service", manifest)
        self.assertIn("usr/bin/listmngr", manifest)
        postinst = read("deploy/packaging/debian/postinst")
        for needle in ["useradd --system", "/etc/listmngr", "/var/lib/listmngr", "nologin"]:
            with self.subTest(needle=needle):
                self.assertIn(needle, postinst)
                self.assertIn(needle, manifest)  # the RPM pre-install script does the same

    def test_the_chart_is_hardened_and_probes_the_binary(self):
        values = (CHART / "values.yaml").read_text(encoding="utf-8")
        deployment = (CHART / "templates/deployment.yaml").read_text(encoding="utf-8")
        for needle in ["runAsNonRoot: true", "readOnlyRootFilesystem: true", 'drop: ["ALL"]', "seccompProfile"]:
            with self.subTest(needle=needle):
                self.assertIn(needle, values)
        for needle in ["path: /healthz", "path: /readyz", "name: migrate", "type: Recreate", "envFrom:"]:
            with self.subTest(needle=needle):
                self.assertIn(needle, deployment)
        self.assertIn("name: listmngr", (CHART / "Chart.yaml").read_text(encoding="utf-8"))
        if shutil.which("helm") is None:
            self.skipTest("helm is not installed here; CI's helm job lints")
        password = ["--set", "postgresql.auth.password=lint-only"]  # required since P10-HELM-DB
        lint = subprocess.run(["helm", "lint", str(CHART), *password], capture_output=True, text=True)
        self.assertEqual(lint.returncode, 0, lint.stdout + lint.stderr)
        for extra in ([], ["--set", "secretFiles.dkim\\.pem=KEY", "--set", "ingress.enabled=true"]):
            rendered = subprocess.run(["helm", "template", "t", str(CHART), *password, *extra], capture_output=True, text=True)
            self.assertEqual(rendered.returncode, 0, rendered.stdout + rendered.stderr)

    def test_ci_lints_the_chart_on_every_push(self):
        workflow = read(".github/workflows/ci.yml")
        self.assertIn("helm lint --strict deploy/helm/listmngr", workflow)
        self.assertIn("helm template", workflow)

    def test_the_upgrade_policy_is_written(self):
        upgrade = read("docs/UPGRADE.md")
        for needle in ["backup", "restore", "migrate", "doctor", "schema_mismatch", "only **adds**"]:
            with self.subTest(needle=needle):
                self.assertIn(needle, upgrade)

    def test_the_release_profile_is_set(self):
        manifest = read("Cargo.toml")
        profile = manifest[manifest.index("[profile.release]"):manifest.index("[profile.release.build-override]")]
        for needle in ["overflow-checks = true", 'lto = "thin"', "codegen-units = 1", "strip = "]:
            with self.subTest(needle=needle):
                self.assertIn(needle, profile)


if __name__ == "__main__":
    unittest.main()
