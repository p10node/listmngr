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
- **Image**: `deploy/Dockerfile` built for `linux/amd64` and
  `linux/arm64`, pushed to GHCR as `<version>` and `<major>.<minor>`,
  signed keyless with cosign by digest.
- **Chart**: `deploy/helm/listmngr`, linted, rendered and packaged
  with the release.
- **SBOM**: CycloneDX JSON of the binary's locked dependency graph
  (`cargo cyclonedx`).
- **Checksums**: one `SHA256SUMS` over everything above, signed keyless
  with `cosign sign-blob` (`SHA256SUMS.sigstore.json`); the image
  digest in the release notes.

```sh
cosign verify-blob --bundle SHA256SUMS.sigstore.json \
  --certificate-identity-regexp 'github.com/p10node/listmngr' \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com SHA256SUMS
sha256sum -c SHA256SUMS --ignore-missing
```

Until the first tag, build from source as the [Install](install.md)
chapter says. Upgrading and rolling back are `docs/UPGRADE.md`.
