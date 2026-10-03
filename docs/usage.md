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
| `--simple-worker-num` | `10` | Simpleモードで同時に実行するHTTPリクエスト数 |
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
# exclude_selectors = [".timestamp", ".advertisement"]
# normalize_whitespace = true
# poll_interval_minutes = 15

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
| `exclude_selectors` | いいえ | 抽出対象要素の内側から除外するCSSセレクターの配列。省略時は除外しません |
| `normalize_whitespace` | いいえ | `true`の場合、抽出テキスト内の連続する空白・改行を1つの半角スペースにまとめます。省略時は`false` |
| `poll_interval_minutes` | いいえ | この対象の巡回間隔（分）。省略時はCLIの`--interval-minutes`を使います。最小値は1分で、全体の巡回周期より短い値も全体周期ごとの判定になります |

URLとCSSセレクターは設定読込時に形式を検査します。SimpleモードはHTTPでHTMLを取得してセレクターに一致する要素のテキストを抽出します。FullモードはヘッドレスChromiumを使うため、JavaScriptで描画されるページに向いています。`wait_seconds`はFullモードでのみ使われます。

`exclude_selectors`は各抽出対象要素の子孫に適用され、一致した要素とその内容を取り除きます。抽出対象要素自身は除外対象になりません。`normalize_whitespace`は抽出後の文字列に適用されるため、見た目上の空白や改行の差による変更通知を抑えられます。どちらも省略時は従来の比較方法を保ちます。これらを有効化または変更すると比較対象の文字列が変わるため、次回取得で一度`changed`通知が送られる場合があります。

`poll_interval_minutes`を指定すると、その対象の最終試行時刻から指定時間が経過した巡回サイクルで取得します。まだ試行していない対象は次のサイクルで取得します。値を省略した対象は、従来どおり毎回の全体サイクルで取得します。`--once`は対象別間隔に関係なく全対象を一度ずつ取得します。

## 保存と変更判定

取得テキストの前後の空白を除き、SHA-256ハッシュで前回の内容と比較します。空でない内容を取得した場合、内容が変われば`last_updated`を更新し、変わらなくても`last_checked`を更新します。空のテキストは内容変更の判定から除外し、ハッシュと`last_updated`は変更しません。

巡回状態は`data.toml`にテーブル形式で保存されます。各項目にはハッシュ、最終変更時刻、最終確認時刻のほか、最終試行・成功時刻、連続失敗数、直近のエラーが含まれます。取得失敗は同じ巡回内で最大3回試行した後に記録します。巡回結果はまとめて保存され、失敗した対象は次回以降の成功で失敗数とエラーが消去されます。以前の形式の`data.toml`に新しい項目がなくても読み込めます。

空の取得テキストはハッシュや`last_checked`を更新しませんが、成功した取得として最終試行・成功時刻を記録します。保存状態は起動時に読み込まれ、そのプロセス内のキャッシュを更新して保存します。

設定ファイルは各巡回サイクルの開始時に再読み込みされるため、対象の追加・変更・削除は次のサイクルから反映されます。編集中などに設定が不正な場合はエラーをログに出し、最後に正常に読み込めた設定で巡回を続けます。

端末には設定対象ごとの状態、最終変更時刻、URLを表示します。状態は`ok`、`failed (連続失敗数)`、`not checked`です。最終変更時刻は更新から1時間以内なら緑、1日以内なら黄、それより前または未変更なら暗色で表示されます。

## WebSocket通知

プロセス起動時に`0.0.0.0:3000`でWebSocketサーバーを開始します。`ws://localhost:3000/`に接続すると、変更・失敗・復旧をJSONメッセージで受け取れます。内容が変わった場合は次の形式です。

接続URLに`id`と`event`を指定すると、対象とイベントを絞れます。各条件は省略可能で、省略した条件は全てを対象にします。

```text
ws://localhost:3000/?id=ProjectReadme&event=changed
```

`event`には`changed`、`poll_failed`、`poll_recovered`を指定できます。不明な値はHTTP 400で拒否されます。簡易WebUIの「最近の通知」にも同じ絞り込みを追加しました。状態一覧にはフィルターを適用せず、通知ストリームだけを絞ります。

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

状態の保存に成功した後、イベントを接続中のクライアントへ配信します。空でない取得結果の保存は巡回内でまとめて行うため、`changed`と`poll_recovered`はその保存後に届きます。接続時点の状態一覧や過去イベントは送信しません。クライアントの受信が大きく遅れた場合は、直近100件より古いイベントを飛ばして配信を続けます。失敗状態は`data.toml`から起動時に復元するため、再起動後に復旧した場合も通知します。時刻文字列はプロセスのローカルタイムゾーンで表示されます。

## WebUIと状態API

簡易WebUIは`http://localhost:3000/ui`で開けます。巡回状態は15秒ごとに再取得され、開いている間に届いたWebSocket通知も表示します。

`GET /api/v1/status`は設定対象の現在状態をJSON配列で返します。時刻はUnixミリ秒です。

```json
[
  {
    "id": "ProjectReadme",
    "url": "https://example.com/project",
    "status": "ok",
    "last_updated_unix_ms": 1791027296000,
    "last_checked_unix_ms": 1791027296000,
    "last_attempted_unix_ms": 1791027296000,
    "last_success_unix_ms": 1791027296000,
    "consecutive_failures": 0,
    "last_error": null
  }
]
```

状態APIとWebSocketには認証がありません。既定のサーバーは全インターフェースで待ち受けるため、信頼できるネットワーク内で利用してください。
