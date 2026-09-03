# ADR-0001: Database-backed queue

- Status: Accepted
- Date: 2026-09-04

## Context
Filesystem pickle queues complicate crash recovery and multi-node operation.

## Decision
Use SQL-backed jobs and a message-store reference. PostgreSQL uses transactional claims with `SKIP LOCKED`; SQLite is explicitly single-node. Queue work begins in Phase 2; Phase 1 establishes portable database conventions.

## Consequences
Claims and side effects can be atomic and observable, but PostgreSQL and SQLite require a shared contract suite and backend-specific claim implementations.
