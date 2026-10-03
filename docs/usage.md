# 利用方法

## ビルド

`src/lib.rs`がnightly専用のfeature gateを使うため、nightly toolchainでビルドします。

```sh
cargo +nightly build --release
```

`config.example.toml`をコピーし、巡回対象に合わせて編集します。

```sh
cp config.example.toml config.toml
./target/release/patrol --config-path ./config.toml --data-path ./data.toml
```

`data.toml`は巡回結果の保存先です。存在しなければ作成されます。設定と記録の既定パスは、それぞれ`./config.toml`と`./data.toml`です。

## コマンドライン引数

| 引数 | 既定値 | 説明 |
| --- | --- | --- |
| `-c`, `--config-path` | `./config.toml` | 巡回設定ファイル |
| `-d`, `--data-path` | `./data.toml` | 巡回状態の保存ファイル |
| `-p`, `--worker-num` | `10` | Fullモードで使うPlaywrightページ数 |
| `-i`, `--interval-minutes` | `1` | 巡回間隔（分）。0を指定しても1分として扱う |
| `--once` | 無効 | 起動後に1回だけ巡回して終了 |

通常起動中は標準入力に`q`を入力すると終了します。ログレベルは`RUST_LOG`で設定できます。例：

```sh
RUST_LOG="patrol=debug" ./target/release/patrol --config-path ./config.toml --data-path ./data.toml --interval-minutes 5
```

## 設定ファイル

トップレベルの各テーブルが1件の巡回設定です。テーブル名は識別子になり、更新一覧や通知に表示されます。

```toml
[ProjectReadme]
url = "https://example.com/project"
selector = "main article"
mode = "full"
wait_seconds = 2

[StaticPage]
url = "https://example.com/status"
selector = ".status-message"
mode = "simple"
```

| 項目 | 必須 | 説明 |
| --- | --- | --- |
| `url` | はい | ページURL |
| `selector` | はい | 抽出する要素を指定するCSSセレクター |
| `mode` | いいえ | `full`または`simple`。省略時は`full` |
| `wait_seconds` | いいえ | Fullモードでセレクターを待つ前に加える待機時間（秒）。セレクター待機自体は最大30秒 |

URLとCSSセレクターは設定読込時に形式を検査します。SimpleモードはHTTPでHTMLを取得してセレクターに一致する要素のテキストを抽出します。FullモードはヘッドレスChromiumを使うため、JavaScriptで描画されるページに向いています。`wait_seconds`はFullモードでのみ使われます。

## 保存と変更判定

取得テキストの前後の空白を除き、SHA-256ハッシュで前回の内容と比較します。空でない内容を取得した場合、内容が変われば`last_updated`を更新し、変わらなくても`last_checked`を更新します。空のテキストは無視され、保存状態も変更されません。

巡回状態は`data.toml`にテーブル形式で保存されます。各項目にはハッシュ、最終変更時刻、最終確認時刻が含まれます。ファイルは起動時に読み込まれ、そのプロセス内のキャッシュを更新して保存します。実行中に設定ファイルを編集しても、変更は次回起動まで読み直されません。

端末には最終変更時刻順の一覧を表示します。更新から1時間以内は緑、1日以内は黄、それより前は暗色で表示されます。

## WebSocket通知

プロセス起動時に`0.0.0.0:3000`でWebSocketサーバーを開始します。`ws://localhost:3000/`に接続すると、変更検出時に次の形のJSONメッセージを受け取れます。

```json
{"id":"ProjectReadme","url":"https://example.com/project","timestamp":"2026-10-03 12:34:56"}
```

通知は起動後に検出した変更だけを流し、接続時点の状態一覧は送信しません。時刻文字列はプロセスのローカルタイムゾーンで表示されます。
