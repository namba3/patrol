# Repository guidance

## Project shape

- This is a Rust 2024 project with an MSRV of Rust 1.89; keep code compatible with stable Rust.
- Keep the onion-architecture boundary: domain types and traits belong in `src/domain/`, use cases in `src/application/`, concrete I/O adapters in `src/infrastructure/`, and process wiring/CLI in `src/main.rs`.
- Application code should depend on domain traits rather than TOML, HTTP, or Playwright implementations.
- `DataRepository::update` returns a timestamp only when the content hash changes. Empty extracted content does not change the saved content hash, but it records a successful poll status.
- Poll failures are recorded after the in-cycle retries. Successful polls clear the consecutive failure count and last error.
- Configuration is reloaded at the start of each patrol cycle; a failed reload keeps the last valid configuration active.
- `DataRepositoryActor` exists but is not part of the current startup path in `src/main.rs`.

## Documentation

- Treat `docs/` as the primary project documentation. Start with `docs/README.md` and update the relevant detailed page when behavior or architecture changes.
- Keep the root `README.md` as a short overview and quick-start page that links to `docs/`.
- Describe current code behavior accurately. Label unverified runtime behavior and future ideas clearly; do not present them as confirmed functionality.

## Changes

- Preserve unrelated user changes and inspect the working tree before editing.
- Avoid changing public contracts or persisted TOML formats unless the task calls for it; document any such change.
- Keep edits focused on the requested scope and avoid introducing dependencies for documentation-only work.
- For every change, consider its effects on maintainability, performance, and UI/UX, and make relevant improvements within scope. Keep the design simple, and base performance optimizations on evidence.

## Privacy and file paths

- Do not commit personal information, credentials, or other private data. Use clearly fictional examples or placeholders when examples are needed.
- Do not include machine-specific absolute file paths in repository files, documentation, examples, or logs. Prefer repository-relative paths or portable commands.
- When personal information or an absolute file path is found, remove it or replace it with a non-identifying placeholder, and check nearby content for related disclosures before committing.

## Code Review

When performing a code review, read and follow [REVIEW.md](./REVIEW.md).
These guidelines apply only to review tasks; regular development work follows the instructions in this file.
