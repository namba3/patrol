# Patrol

[日本語](README.md) | English

A Rust tool that periodically fetches selected parts of web pages and records when their content changes. Targets are specified by URL and CSS selector.

This is a personal learning project for building an onion architecture in Rust. See [`docs/`](docs/README.md) for usage and design details.

## Quick start

Build with the stable Rust toolchain (Rust 1.89 or newer).

```sh
cargo build --release
cp config.example.toml config.toml
./target/release/patrol --config-path ./config.toml --data-path ./data.toml
```

By default, Patrol checks the configured pages every minute. Enter `q` or close standard input to exit. Add `--once` to run a single check and exit.

After startup, open the status page at [http://localhost:3000/ui](http://localhost:3000/ui).

## Documentation

- [Documentation index](docs/README.md)
- [Usage and configuration](docs/usage.md)
- [Architecture and runtime flow](docs/architecture.md)
- [Development guide](docs/development.md)

## License

This project is available under the terms of either the [MIT License](LICENSE-MIT) or the [Apache License 2.0](LICENSE-APACHE).
