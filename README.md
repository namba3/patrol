# Patrol

日本語 | [English](README.en.md)

指定したWebページの一部を定期的に取得し、内容が変わった時刻を記録するRust製ツールです。取得対象はURLとCSSセレクターで指定します。

このリポジトリは、Rustでオニオンアーキテクチャを組み立てる学習用の個人プロジェクトです。使い方や設計の説明は [`docs/`](docs/README.md) を参照してください。

## クイックスタート

Rust 1.89以降のstable toolchainとDioxus CLI 0.7.10でビルドします。詳細は[利用方法](docs/usage.md)を参照してください。

```sh
rustup target add wasm32-unknown-unknown
cargo install dioxus-cli --version 0.7.10 --locked
./scripts/build_ui.sh
cargo build --release
cp config.example.toml config.toml
./target/release/patrol --config-path ./config.toml --data-path ./data.toml
```

初期状態では1分ごとに巡回します。`q`を入力するか標準入力を閉じると終了します。1回だけ巡回する場合は`--once`を指定してください。

簡易状態画面は起動後に [http://localhost:3000/ui](http://localhost:3000/ui) で開けます。

## 資料

- [資料一覧](docs/README.md)
- [利用方法と設定ファイル](docs/usage.md)
- [アーキテクチャと処理の流れ](docs/architecture.md)
- [開発ガイド](docs/development.md)

## ライセンス

このプロジェクトは [MIT License](LICENSE-MIT) または [Apache License 2.0](LICENSE-APACHE) の条件で利用できます。
