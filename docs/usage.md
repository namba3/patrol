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

取得テキストの前後の空白を除き、SHA-256ハッシュで前回の内容と比較します。空でない内容を取得した場合、内容が変われば`last_updated`を更新し、変わらなくても`last_checked`を更新します。空のテキストは内容変更の判定から除外し、ハッシュと`last_updated`は変更しません。

巡回状態は`data.toml`にテーブル形式で保存されます。各項目にはハッシュ、最終変更時刻、最終確認時刻のほか、最終試行・成功時刻、連続失敗数、直近のエラーが含まれます。取得失敗は同じ巡回内で最大3回試行した後に記録します。失敗した対象は、次回以降の成功で失敗数とエラーが消去されます。以前の形式の`data.toml`に新しい項目がなくても読み込めます。

空の取得テキストはハッシュや`last_checked`を更新しませんが、成功した取得として最終試行・成功時刻を記録します。保存状態は起動時に読み込まれ、そのプロセス内のキャッシュを更新して保存します。

設定ファイルは各巡回サイクルの開始時に再読み込みされるため、対象の追加・変更・削除は次のサイクルから反映されます。編集中などに設定が不正な場合はエラーをログに出し、最後に正常に読み込めた設定で巡回を続けます。

端末には設定対象ごとの状態、最終変更時刻、URLを表示します。状態は`ok`、`failed (連続失敗数)`、`not checked`です。最終変更時刻は更新から1時間以内なら緑、1日以内なら黄、それより前または未変更なら暗色で表示されます。

## WebSocket通知

プロセス起動時に`0.0.0.0:3000`でWebSocketサーバーを開始します。`ws://localhost:3000/`に接続すると、変更・失敗・復旧をJSONメッセージで受け取れます。内容が変わった場合は次の形式です。

```json
{"event":"changed","id":"ProjectReadme","url":"https://example.com/project","timestamp":"2026-10-03 12:34:56"}
```

同じ対象で初めて失敗状態になった時に`poll_failed`を送ります。失敗が続いている間は状態を保存しますが、通知を繰り返しません。

```json
{"event":"poll_failed","id":"ProjectReadme","url":"https://example.com/project","timestamp":"2026-10-03 12:34:56","consecutive_failures":1,"error":"request failed"}
```

失敗状態の対象を再び取得できた時は`poll_recovered`を送ります。成功した取得テキストに変更があれば、`changed`も別に送られます。

```json
{"event":"poll_recovered","id":"ProjectReadme","url":"https://example.com/project","timestamp":"2026-10-03 12:34:56"}
```

イベントは発生時に接続中のクライアントへ配信され、接続時点の状態一覧や過去イベントは送信しません。失敗状態は`data.toml`から起動時に復元するため、再起動後に復旧した場合も通知します。時刻文字列はプロセスのローカルタイムゾーンで表示されます。
