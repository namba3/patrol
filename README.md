# Patrol

指定したWebページの一部を定期的に取得し、内容が変わった時刻を記録するRust製ツールです。取得対象はURLとCSSセレクターで指定します。

このリポジトリは、Rustでオニオンアーキテクチャを組み立てる学習用の個人プロジェクトです。使い方や設計の説明は [`docs/`](docs/README.md) を参照してください。

## クイックスタート

Rustのnightly toolchainでビルドします。

```sh
cargo +nightly build --release
cp config.example.toml config.toml
./target/release/patrol --config-path ./config.toml --data-path ./data.toml
```

初期状態では1分ごとに巡回します。`q`を入力すると終了します。1回だけ巡回する場合は`--once`を指定してください。

簡易状態画面は起動後に [http://localhost:3000/ui](http://localhost:3000/ui) で開けます。

## 資料

- [資料一覧](docs/README.md)
- [利用方法と設定ファイル](docs/usage.md)
- [アーキテクチャと処理の流れ](docs/architecture.md)
- [開発ガイド](docs/development.md)
