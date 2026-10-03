# Repository guidance

## Project shape

- This is a Rust 2021 project that currently requires the nightly toolchain because `src/lib.rs` enables nightly features.
- Keep the onion-architecture boundary: domain types and traits belong in `src/domain/`, use cases in `src/application/`, concrete I/O adapters in `src/infrastructure/`, and process wiring/CLI in `src/main.rs`.
- Application code should depend on domain traits rather than TOML, HTTP, or Playwright implementations.
- `DataRepository::update` returns a timestamp only when the content hash changes. Empty extracted content does not change the saved content hash, but it records a successful poll status.
- Poll failures are recorded after the in-cycle retries. Successful polls clear the consecutive failure count and last error.
- `DataRepositoryActor` exists but is not part of the current startup path in `src/main.rs`.

## Documentation

- Treat `docs/` as the primary project documentation. Start with `docs/README.md` and update the relevant detailed page when behavior or architecture changes.
- Keep the root `README.md` as a short overview and quick-start page that links to `docs/`.
- Describe current code behavior accurately. Label unverified runtime behavior and future ideas clearly; do not present them as confirmed functionality.

## Changes

- Preserve unrelated user changes and inspect the working tree before editing.
- Avoid changing public contracts or persisted TOML formats unless the task calls for it; document any such change.
- Keep edits focused on the requested scope and avoid introducing dependencies for documentation-only work.
