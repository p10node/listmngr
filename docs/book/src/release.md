# Releases

A tag `v<version>` runs `.github/workflows/release.yml`, every action
pinned by commit:

- **Binaries**: `cargo build --locked --release` on the pinned
  toolchain for `x86_64-unknown-linux-musl`,
  `aarch64-unknown-linux-musl` (both static), `aarch64-apple-darwin`
  and `x86_64-apple-darwin`; one `.tar.gz` each with the binary, the
  licence, the README, `UPGRADE.md` and the systemd unit.
- **Packages**: a `.deb` (`cargo-deb`) and an `.rpm`
  (`cargo-generate-rpm`) for x86_64 Linux from the musl binary; both
  install `/usr/bin/listmngr`, the unit (disabled), the `listmngr`
  account, `/etc/listmngr` and `/var/lib/listmngr`.
- **Images**: `deploy/Dockerfile` and `deploy/postfix/Dockerfile`, each
  built natively on an x86-64 and an arm64 runner (no emulation), pushed
  to GHCR by digest and joined into one manifest list per image under
  `<version>`, `<major>.<minor>` and `latest`
  (`ghcr.io/p10node/listmngr`, `ghcr.io/p10node/listmngr-postfix`),
  signed keyless with cosign by the list's digest.
- **Chart**: `deploy/helm/listmngr`, linted strictly, rendered and
  packaged with the release, pushed to
  `oci://ghcr.io/p10node/charts/listmngr` as `<version>` and signed
  keyless with cosign by digest; the chart's digest in the release notes.
- **SBOM**: CycloneDX JSON of the binary's locked dependency graph
  (`cargo cyclonedx`).
- **Checksums**: one `SHA256SUMS` over everything above, signed keyless
  with `cosign sign-blob` (`SHA256SUMS.sigstore.json`); the digests of
  the images and the chart in the release notes.

```sh
cosign verify-blob --bundle SHA256SUMS.sigstore.json \
  --certificate-identity-regexp 'github.com/p10node/listmngr' \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com SHA256SUMS
sha256sum -c SHA256SUMS --ignore-missing
cosign verify ghcr.io/p10node/charts/listmngr:<version> \
  --certificate-identity-regexp 'github.com/p10node/listmngr' \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com
```

Until the first tag, build from source as the [Install](install.md)
chapter says. Upgrading and rolling back are `docs/UPGRADE.md`.
