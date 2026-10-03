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

`data.toml`は巡回状態、`history.toml`は変更履歴の保存先です。どちらも存在しなければ作成されます。既定パスは設定が`./config.toml`、巡回状態が`./data.toml`、変更履歴が`./history.toml`です。

## コマンドライン引数

| 引数 | 既定値 | 説明 |
| --- | --- | --- |
| `-c`, `--config-path` | `./config.toml` | 巡回設定ファイル |
| `-d`, `--data-path` | `./data.toml` | 巡回状態の保存ファイル |
| `--history-path` | `./history.toml` | 内容変更履歴の保存ファイル |
| `--history-limit` | `100` | 全対象で保持する変更履歴の上限（1〜1000件） |
| `-p`, `--worker-num` | `10` | Fullモードで使うPlaywrightページ数 |
| `--simple-worker-num` | `10` | Simpleモードで同時に実行するHTTPリクエスト数 |
| `--web-listen` | `0.0.0.0:3000` | HTTP・WebSocketサーバーの待受アドレス |
| `-i`, `--interval-minutes` | `1` | 巡回間隔（分）。0を指定しても1分として扱う |
| `--once` | 無効 | 起動後に1回だけ巡回して終了 |

通常起動中は標準入力に`q`を入力するか、標準入力を閉じるか、Ctrl-Cを押すと終了します。Unix系OSではSIGTERMも終了要求として扱います。終了要求後は実行中の巡回とその保存を完了し、新しい巡回は開始せずに終了します。保留中の変更履歴も書き終えてから終了します。ログレベルは`RUST_LOG`で設定できます。例：

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
| `wait_seconds` | いいえ | Fullモードでセレクターを待つ前に加える待機時間（秒）。ページ遷移とセレクター待機にはそれぞれ最大30秒の上限があります |
| `exclude_selectors` | いいえ | 抽出対象要素の内側から除外するCSSセレクターの配列。省略時は除外しません |
| `normalize_whitespace` | いいえ | `true`の場合、抽出テキスト内の連続する空白・改行を1つの半角スペースにまとめます。省略時は`false` |
| `poll_interval_minutes` | いいえ | この対象の巡回間隔（分）。省略時はCLIの`--interval-minutes`を使います。最小値は1分で、全体の巡回周期より短い値も全体周期ごとの判定になります |

URLとCSSセレクターは設定読込時に形式を検査します。SimpleモードはHTTPでHTMLを取得してセレクターに一致する要素のテキストを抽出します。SimpleモードのHTTPリクエストは応答本文の読み込みを含めて最大30秒でタイムアウトし、本文は16 MiBを上限とします。FullモードはヘッドレスChromiumを使うため、JavaScriptで描画されるページに向いています。`wait_seconds`はFullモードでのみ使われます。

`exclude_selectors`は各抽出対象要素の子孫に適用され、一致した要素とその内容を取り除きます。抽出対象要素自身は除外対象になりません。`normalize_whitespace`は抽出後の文字列に適用されるため、見た目上の空白や改行の差による変更通知を抑えられます。どちらも省略時は従来の比較方法を保ちます。これらを有効化または変更すると比較対象の文字列が変わるため、次回取得で一度`changed`通知が送られる場合があります。

`poll_interval_minutes`を指定すると、その対象の最終試行時刻から指定時間が経過した巡回サイクルで取得します。まだ試行していない対象は次のサイクルで取得します。値を省略した対象は、従来どおり毎回の全体サイクルで取得します。`--once`は対象別間隔に関係なく全対象を一度ずつ取得します。

## 保存と変更判定

取得テキストの前後の空白を除き、SHA-256ハッシュで前回の内容と比較します。空でない内容を取得した場合、内容が変われば`last_updated`を更新し、変わらなくても`last_checked`を更新します。空のテキストは内容変更の判定から除外し、ハッシュと`last_updated`は変更しません。

巡回状態は`data.toml`にテーブル形式で保存されます。各項目にはハッシュ、最終変更時刻、最終確認時刻のほか、最終試行・成功時刻、連続失敗数、直近のエラーが含まれます。取得失敗は巡回サイクルの締切までに最大3回試行し、再試行の間には250ms、500msの待ち時間を設けます。締切までに待ち時間を終えられない場合は再試行しません。巡回結果はまとめて保存され、失敗した対象は次回以降の成功で失敗数とエラーが消去されます。以前の形式の`data.toml`に新しい項目がなくても読み込めます。

空の取得テキストはハッシュや`last_checked`を更新しませんが、成功した取得として最終試行・成功時刻を記録します。保存状態は起動時に読み込まれ、そのプロセス内のキャッシュを更新して保存します。

設定ファイルは各巡回サイクルの開始時に再読み込みされるため、対象の追加・変更・削除は次のサイクルから反映されます。編集中などに設定が不正な場合はエラーをログに出し、最後に正常に読み込めた設定で巡回を続けます。

端末には設定対象ごとの状態、最終変更時刻、URLを表示します。状態は`ok`、`failed (連続失敗数)`、`not checked`です。最終変更時刻は更新から1時間以内なら緑、1日以内なら黄、それより前または未変更なら暗色で表示されます。

## WebSocket通知

プロセス起動時に既定では`0.0.0.0:3000`でHTTP・WebSocketサーバーを開始します。`--web-listen 127.0.0.1:3000`のように指定すると待受先を変更できます。`ws://localhost:3000/`に接続すると、変更・失敗・復旧をJSONメッセージで受け取れます。内容が変わった場合は次の形式です。

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

簡易WebUIは`http://localhost:3000/ui`で開けます。巡回状態は15秒ごとに再取得され、開いている間に届いたWebSocket通知も表示します。WebSocketが切断されると1秒後から再接続を試し、失敗が続く場合は最大30秒間隔まで延長します。接続できると間隔を1秒に戻します。

`GET /healthz`はWebサーバーのliveness確認用で、応答時に`200 OK`と`ok`を返します。巡回対象が正常かどうかは示さないため、対象状態の監視には状態APIを使ってください。

`GET /api/v1/status`は設定対象の現在状態をIDの昇順でJSON配列として返します。時刻はUnixミリ秒です。起動直後は保存済み状態を使い、最初の巡回が終わる前に一覧を公開します。初回巡回の保存後に最新状態へ更新されます。

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

`GET /api/v1/history`は内容変更履歴を新しい順でページ単位に返します。`?id=ProjectReadme`を指定すると対象を絞り込めます。`limit`は1ページの件数（既定50、最大100）、`offset`は新しい履歴から読み飛ばす件数です。`offset`が絞り込み後の全件数以上の場合は最後のページに調整します。レスポンスにはページの`entries`に加えて、絞り込み後の全件数`total`と前後ページの有無`has_older`・`has_newer`が含まれます。簡易WebUIの巡回対象表にある「表示」ボタンから対象ごとの履歴をページ送りで確認でき、通信に失敗した場合は履歴画面から再試行できます。履歴は`history.toml`へ`data.toml`とは別に保存されます。

保存するのは内容が変わった時の新旧テキストです。履歴は全対象で直近100件まで保持します。`--history-limit`で1〜1000件に変更できます。各本文は先頭4 KiBまでです。上限を超えた本文には`content_truncated`または`previous_truncated`が付きます。WebUIでは変更行を行単位で色分けします。比較する行数が多い場合は本文表示に切り替え、4 KiBを超えた履歴は保存された先頭部分同士を比較します。履歴機能の導入前の本文は復元できないため、最初の記録では`previous_content`が`null`になる場合があります。本文はHTMLとしてではなくテキストとして表示します。

```json
{
  "entries": [
    {
      "id": "ProjectReadme",
      "timestamp_unix_ms": 1791027296000,
      "previous_content": "旧テキスト",
      "previous_truncated": false,
      "content": "新テキスト",
      "content_truncated": false
    }
  ],
  "offset": 0,
  "limit": 50,
  "total": 1,
  "has_older": false,
  "has_newer": false
}
```

状態APIとWebSocketには認証がありません。既定のサーバーは全インターフェースで待ち受けるため、信頼できるネットワーク内で利用してください。
