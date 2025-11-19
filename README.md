# Patrol

Periodically patrol the website to see if it has been updated.

This is my private project.

This project is also for me to learn how to implement the onion architecture in Rust.

## Prerequires

- Rust installed

## Build

```sh
cargo +nightly build --release
```

## Run

```sh
RUST_LOG="patrol=DEBUG" ./target/release/patrol -c ./config.example.toml -d ./data.toml -w 10
```
