# 0002 — TypeScript modular cloud monolith (Fastify + Next.js)

## Status

Accepted

## Context

The cloud control plane is a conventional web workload — auth, organizations, devices, entitlements, billing, releases, policies (build-doc §18) — built by a small team, and build-doc §6 explicitly starts the cloud as a modular monolith with service extraction allowed only for a concrete reason. Decisions D25, D26, and D31 lock the framework choices.

## Decision

Build the cloud as a TypeScript modular monolith: a Fastify control-plane API plus Next.js frontends (public site, account app, admin console), organized as pnpm workspaces with turborepo per the build-doc §7 `apps/` and `packages/` layout.

## Consequences

- One language and one deploy unit across API and frontends; `packages/shared-types` and `packages/api-client` keep contracts consistent.
- The `schemas/cloud-api/v1.json` contract (03-interfaces §8) is the only coupling to the Rust local runtime, so either side can evolve behind versioned schemas.
- Service extraction is deferred until scaling, security isolation, or team ownership demands it, avoiding premature microservice overhead.
- The monolith must still enforce internal module boundaries (auth, billing, releases) to keep later extraction possible.
