# Patrol

Periodically patrol the website to see if it has been updated.

This is my private project.

This project is also for me to learn how to implement the onion architecture in Rust.

## Prerequires

- Rust installed
- Any proxies for WebDriver installed
  - chromedriver (when you use google chrome)
  - geckodriver (when you use firefox)

## Build

```sh
cargo +nightly build --release
```

## Run

### Start WebDriver

```sh
chromedriver --port=PORT # when you use google chrome
geckodriver --port=PORT  # when you use firefox
```

### Run patrol

```sh
RUST_LOG="patrol=DEBUG" ./target/release/patrol -c ./config.example.toml -d ./data.toml -p PORT
```
