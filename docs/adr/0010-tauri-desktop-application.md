# 0010 — Tauri desktop application

## Status

Accepted

## Context

The desktop app (build-doc §8.3) is a management surface — onboarding, integration control, dashboards, Compression Explorer — over a runtime that already exists in Rust. Shipping a second runtime (Electron's bundled Chromium and Node) would inflate installer size and duplicate logic that the supervisor already exposes. Decisions D24 and the build-doc §2 assumption lock Tauri, built in a later phase (Phase 5).

## Decision

Build the desktop application with Tauri, embedding the existing Rust runtime directly and rendering the UI with web technology from the shared `packages/ui` workspace.

## Consequences

- Small signed binaries and direct in-process access to the supervisor, engine, and store — no IPC shim to a separate runtime.
- UI code and design system are shared with the web apps, keeping one frontend skill set across the team.
- Platform webviews (WebKit, WebView2) differ, so the platform test matrix (build-doc §25.5) must cover rendering and update behavior per OS.
- Desktop work is deferred to Phase 5; CLI and MCP deliver the complete local product before any UI exists.
