# 0012 — Remote telemetry not built in slice, opt-in by future decision

## Status

Accepted

## Context

Build-doc §21.3 makes remote telemetry opt-in and bans prompts, source code, file paths, repository names, symbols, patches, tool arguments, and provider keys from ever being included. Decision D6 is an owner hard constraint that goes further for the current build: no remote telemetry is built and none is enabled; statistics are local-only.

## Decision

The slice ships no telemetry code paths: no event batching, no upload client, and no calls to `POST /v1/events`. All savings and operational statistics live in local SQLite and are surfaced by `compresso stats` and the dashboard. Any future telemetry requires a new explicit owner decision and remains opt-in with the §21.3 exclusions and inspectable sample payloads.

## Consequences

- There is no telemetry attack or leak surface in shipped builds; the privacy claim is verifiable by reading the code, not just the settings.
- The cloud `event_batches` table and telemetry allowlist schema exist as contracts only (02-schemas §5) and stay unused by slice builds.
- Product and quality decisions rely on local dogfooding (D9) and the evaluation corpus instead of field metrics.
- Diagnostics remain user-controlled local exports (build-doc §24.1); support workflows must work from user-provided data.
