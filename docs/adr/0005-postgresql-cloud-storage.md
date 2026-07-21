# 0005 — PostgreSQL cloud storage

## Status

Accepted

## Context

The control plane manages relational, money-adjacent data — users, organizations, subscriptions, entitlements, devices, releases, audit logs (build-doc §18.2) — where referential integrity and mature tooling matter. Decision D30 locks PostgreSQL; D27 leaves the managed provider (Fly Postgres or Neon) open because the contracts are unaffected.

## Decision

Use PostgreSQL for all cloud storage, with the DDL in `docs/plan/02-schemas.md` §5 as the authoritative v1 schema and a Drizzle schema mirroring it in the TypeScript workspace.

## Consequences

- Foreign keys, CHECK constraints, and append-only audit tables directly encode billing and entitlement invariants.
- The privacy invariant — no column stores source code, prompts, file paths, repository names, symbols, patches, or raw tool arguments — is testable in Phase 6 CI against the schema.
- Managed hosting keeps operational burden on the single infra/security engineer acceptable; the provider choice can be made at provisioning time.
- Better Auth and Stripe integrations (D28, D29) both have first-class PostgreSQL support, avoiding custom persistence glue.
