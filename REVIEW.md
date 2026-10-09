# AI Code Review Guidelines

This document supplements `AGENTS.md` for code review tasks. It describes review criteria; it does not replace the repository's development instructions.

## Review Principles

- Prioritize concrete correctness, security, and reliability defects in the proposed change and its effects on callers and persisted or public interfaces.
- Trace relevant call paths and consult the domain contracts and `docs/` when a local code excerpt is not enough to establish behavior.
- Base findings on code, configuration, or reproducible behavior. State the triggering conditions and distinguish confirmed defects from questions that need verification.
- Do not treat stylistic preference or hypothetical risk as a defect.

## Review Priorities

1. **Correctness** — polling, change detection, status transitions, history, and API/UI behavior.
2. **Security** — exposure of the unauthenticated web endpoints, fetched content, and user-controlled URLs.
3. **Reliability** — persistence consistency, retries, timeouts, configuration reload, and orderly shutdown.
4. **Concurrency and resource safety** — task cancellation, bounded queues, browser-page ownership, and memory/response limits.
5. **Compatibility** — Rust 1.89 MSRV, existing TOML files, CLI behavior, HTTP/JSON/WebSocket contracts, and UI asset bundling.
6. **Performance** — avoid unbounded work, retained content, or blocking work on async executor threads.
7. **Maintainability** — preserve layer boundaries and keep docs aligned with actual behavior.

## Project-Specific Review Guidelines

### Architecture and domain contracts

- Keep models and repository/poller traits in `src/domain/`, orchestration in `src/application/`, concrete I/O in `src/infrastructure/`, and process wiring in `src/main.rs`. Application logic must not depend on TOML, HTTP, or Playwright implementations.
- `Poller::poll_multiple` yields results in completion order, not configuration order. Match results by `Id`; do not pair them with input positions.
- `DataRepository::update` returns a timestamp only when the content hash changes. Empty extracted text leaves the saved hash and `last_checked` unchanged, while recording a successful attempt; a successful poll clears prior consecutive failures and the last error.
- Configuration reload happens at the beginning of each patrol cycle. A parse/read failure must leave the last valid configuration active.
- `DataRepositoryActor` exists but is not used by the startup path. Do not assume it serializes current production repository access.

### Polling, concurrency, and shutdown

- A failed item is attempted at most three times within a cycle, with 250 ms and 500 ms retry delays included in the cycle deadline. Check that work does not start after the deadline and that failures are recorded only after retries finish.
- Simple polling uses HTTP with a 30-second request timeout and a 16 MiB response-body cap, including chunked responses. HTML extraction runs in `spawn_blocking`; preserve response-size and concurrency bounds.
- Full polling uses a pooled set of Playwright pages. Navigation and selector waits have 30-second timeouts; result-stream cancellation must stop its producer and release page/browser resources. Simple-only configurations must not require launching Chromium.
- Shutdown must allow the current cycle's results to be saved, drain queued history writes, stop notification forwarding, close WebSockets, and then stop the HTTP server. Review bounded-channel backpressure and task-error paths together; an early return must not skip cleanup.
- Poll results, notifications, and history batches can complete asynchronously. Preserve ID association, persistence-before-publication guarantees, and behavior when a receiver closes or a bounded queue is full.

### Persistence and history

- TOML configuration, status, and history are persistent user data. Preserve backward-compatible defaults when adding fields and avoid silently changing their meaning or format.
- `TomlFileProxy` writes a same-directory temporary file and renames it into place while preserving file permissions. Repository implementations update caches before saving and roll them back on failure; review both the disk state and in-memory state on error paths.
- History is globally limited (default 100, configurable up to 1,000 entries); each stored content snapshot is capped at 4 KiB on a UTF-8 boundary and records whether it was truncated. Keep retention, previous/current content links, rollback, and UI/API snapshots consistent.
- History-write failures are retried up to three times. Verify that notification and shutdown behavior remains consistent with the persisted result and the current documented contract.

### Web/API and security

- The web server defaults to `0.0.0.0:3000`. Status, history, and WebSocket endpoints have no authentication. Status data includes configured URLs and error details; history exposes captured page text. Treat changes to these routes, fields, binding defaults, or logs as security-sensitive and avoid expanding exposure unintentionally.
- WebSocket `id` and `event` query parameters filter events for a connection; they are not authorization controls. `/healthz` is a liveness check, not a patrol-health result.
- Keep history pagination bounded and serve only assets found in the embedded asset index. Do not turn URL-derived paths into arbitrary filesystem access.
- Poll targets come from configuration. If a change lets an untrusted party control that configuration or trigger requests, assess access to loopback, private-network, and local services under that new threat model; do not report this as a defect based only on a trusted local configuration.
- UI links should remain restricted to HTTP/HTTPS. CSV exports must retain spreadsheet-formula neutralization. UI changes should keep Japanese/English catalog keys aligned, use complete translated messages, preserve locale fallback, and keep the documented default sort (`last_updated`, newest change first) after reset as well as on first load.

### Compatibility, build, and validation

- The main crate uses Rust 2024 and declares Rust 1.89 as its MSRV. CI uses stable, so review newly used APIs and dependency requirements against the declared MSRV rather than assuming the latest stable is sufficient.
- TOML files, CLI arguments, status JSON timestamps (Unix milliseconds), history response pagination, and WebSocket event fields are compatibility surfaces. Changes need matching migration/fallback behavior and documentation.
- The Dioxus UI is a separate crate in `ui/`, not a member of the root Cargo workspace. `./scripts/build_ui.sh` produces assets under `web/dist/`, which `build.rs` embeds into the main binary. A root-only Cargo build can succeed without compiling the UI; the current CI workflow does not run the UI bundle build. Treat core CI success as insufficient evidence for a UI change.
- Existing automated CI runs formatting, Clippy with warnings denied, tests, and a release build, while skipping Playwright driver downloads. It does not verify Chromium startup or live target-site behavior. Keep source-level, bundle/build, browser, and live-site evidence distinct.

## Findings and Severity

Report findings from highest to lowest severity. Use the repository's established review-system format if one is introduced; otherwise use this format:

```text
[P1] Short, actionable summary
`src/path/file.rs:123`
Problem: ...
Condition: ...
Impact: ...
Suggested fix: ...
```

Use a narrow line location in the changed code where possible. Explain the evidence and triggering condition; make the impact and suggested fix specific. Do not combine unrelated problems into one finding.

- **P0 — Critical:** A release-blocking issue with severe impact, such as a directly exploitable security flaw or widespread irreversible data loss.
- **P1 — High:** A serious security, data-integrity, or core-functionality failure in a normal supported deployment, with no reasonable workaround.
- **P2 — Medium:** A concrete defect limited to a supported edge case or a substantial but recoverable degradation.
- **P3 — Low:** A small, user-visible defect with limited impact. Include only when the correction is actionable and worthwhile.

Do not assign severity based only on theoretical worst-case impact. Consider likelihood, affected users, deployment assumptions, and available workarounds.

## Review Exclusions

- Do not report issues that the configured formatter or linter would mechanically identify, unless they reveal a separate functional defect.
- Exclude unsupported assumptions, speculative risks without a concrete trigger, personal style preferences, and low-value refactoring suggestions.
- Do not attribute unrelated pre-existing problems to the proposed change. Mention an existing issue only when it blocks or materially changes the behavior of the reviewed code.
- Do not request documentation or tests solely for completeness; request them when their absence leaves a concrete behavior or compatibility risk unverified.

## Review Behavior

- Do not change code or files during review.
- Separate findings from optional improvement suggestions and questions.
- Respect documented design intent and constraints; when code and docs disagree, verify the implementation and identify the discrepancy rather than treating either as automatically authoritative.
- Inspect relevant callers, callees, persistence/API contracts, and failure paths when a changed function's local context is insufficient.
- Phrase uncertain concerns as concise questions with the missing evidence identified; do not present them as confirmed findings.
- Keep findings concise and actionable, and state when no actionable issue was found.
