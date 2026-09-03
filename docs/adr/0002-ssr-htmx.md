# ADR-0002: Server-rendered UI with htmx

- Status: Accepted
- Date: 2026-09-04

## Context
A full SPA adds a second runtime, duplicated validation and a larger CSP/supply-chain surface.

## Decision
Use Askama server-side templates, vendored htmx and plain CSS. JavaScript evaluation remains disabled; forms retain normal HTTP behavior.

## Consequences
The binary embeds assets and enforces one validation path. Rich interactions require progressive enhancement and careful partial-response/a11y tests in Phase 4.
