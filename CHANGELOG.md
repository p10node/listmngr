# Changelog

## [Unreleased]

### Added
- Phase 0 Rust workspace, CLI, health/readiness/metrics, pinned CI and hardened deployment artifacts; acceptance remains tracked per ID in `docs/FEATURE_PARITY.md`.
- Phase 1 domain model, portable SQLx repositories, REST APIs, scoped tokens, audit log, preferences and compatibility surface; full Phase 1 acceptance is not yet claimed.

### Security
- Static musl/scratch non-root container, filtered Docker context, runtime-only PostgreSQL credentials, hardened systemd unit, and explicit deny/audit policy.
- Full GNU Affero General Public License v3 text and RFC 9116-style `security.txt` disclosure metadata.

### Decisions
- Package version starts at 0.1.0 for unreleased Phase 1 development; it is not a release or phase-completion tag.
- Project license is AGPL-3.0-or-later.
- Available stable dependency versions replace unavailable future PLAN pins; see ADR-0003.
