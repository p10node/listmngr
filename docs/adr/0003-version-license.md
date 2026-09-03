# ADR-0003: Version and license baseline

- Status: Accepted
- Date: 2026-09-04

## Context
The draft PLAN named future/unavailable crate versions and left licensing open.

## Decision
Start the workspace at 0.1.0 and pin compatible stable releases in Cargo.lock. Use AGPL-3.0-or-later. Version changes follow reviewed dependency upgrades, not speculative PLAN numbers.

## Consequences
Hosted modifications must offer corresponding source. The PLAN records manifest/Cargo.lock as the executable version source of truth.
