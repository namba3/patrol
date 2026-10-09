# アーキテクチャ

Patrolは、ドメインの約束を中心にapplicationとinfrastructureを分けるオニオンアーキテクチャの小さな例です。依存の向きは外側の具体実装から内側のdomainに向かいます。

```text
main.rs（組み立て・実行）
  ├── application（巡回手順）
  │     └── domain（型とtrait）
  └── infrastructure（domain traitの実装）
        └── domain
```

## レイヤー

### Domain — `src/domain/`

`Config`、`Data`、`Mode`などのモデルと、アプリケーションが必要とするtraitを定義します。

- `ConfigRepository`: 設定の一覧取得、更新、削除。
- `DataRepository`: 巡回状態の取得、更新、削除。
- `Poller`: 1件または複数件のページ内容取得。
- `Id`、`Url`、`Selector`、`Hash`、`Timestamp`: 値の形式や操作をまとめた型。

DomainはTOMLやHTTPクライアントといった保存・通信手段を選びません。

### Application — `src/application/`

`App`が巡回周期、設定取得、ポーリング、ハッシュ保存、更新一覧の出力を制御します。`SelectivePoller`は`Mode`に応じてFullまたはSimpleのpollerへ処理を振り分け、複数の取得ストリームをまとめます。任意の対象別間隔がある場合は`last_attempted`を使って各サイクルの取得対象を選び、状態一覧を`watch`でWeb層へ渡します。

`DataRepositoryActor`は容量64のbounded channelを介してリポジトリ操作を直列化する補助実装です。キューが埋まると要求元が送信完了を待ちます。現在の`main.rs`の起動経路では使用されていません。

### Infrastructure — `src/infrastructure/`

- `TomlConfigRepository`: 設定をTOMLから読み込み、変更をファイルに保存します。
- `TomlDataRepository`: ハッシュや時刻などの巡回状態をTOMLに保存します。
- `TomlChangeHistoryRepository`: 内容が変わった時の新旧本文を別のTOMLファイルへ保存します。
- `change_history_writer`: 巡回から届く変更履歴batchを保存し、再試行とスナップショット更新を行います。キューが閉じた後は残りのbatchを処理して終了します。
- `TomlFileProxy`: TOMLファイルをメモリ上のキャッシュと同期します。
- `web`: HTTPの状態・履歴API、WebSocket通知、組み込みWebUIを提供します。アプリケーション通知をJSON化してbroadcastへ中継し、watch/broadcastのスナップショットを読み出します。保存処理には依存しません。
- `HttpPoller`: HTTPでHTMLを取得し、CSSセレクターによるCPU処理をブロッキング用スレッドプールで実行します。
- `PlaywrightPoller`: Fullモードの初回巡回時にPlaywrightとChromiumを初期化・準備し、DOM要素のテキストを取得します。初期化に失敗した場合、その失敗はキャッシュせず後続の巡回で再試行します。ページ数を同時実行数と結果キュー容量の上限に使い、結果の消費が遅い場合は巡回側も待機します。結果ストリームが破棄されると生成タスクも停止します。Simpleモードだけを使う場合はブラウザーを起動・準備しません。

### Composition root — `src/main.rs`

CLI引数を解釈し、TOMLリポジトリと2種類のpollerを生成して`App`へ渡します。`infrastructure::web`へ状態・変更履歴スナップショットを渡してWebサーバーを組み立て、`--web-listen`で指定したアドレスでHTTPとWebSocketを待ち受けます。履歴書き込みタスクもInfrastructureへ委譲して起動し、終了時に完了を待ちます。終了要求（`q`、標準入力EOF、Ctrl-C、Unix系OSのSIGTERM）を受けると、現在の巡回と保存を終え、通知中継と履歴書き込みの完了を待ちます。その後WebSocketへcloseを送り、HTTPサーバー終了を最大5秒待ちます。巡回アプリ、Webサーバー、通知中継、履歴書き込みの各taskがエラー終了した場合は、終了処理の完了後にエラーをプロセス終了結果へ反映します。通知中継または履歴書き込みtaskの予期しない終了は稼働中にも監視し、異常を検知すると巡回アプリへ終了要求を送ります。

## 巡回の流れ

1. `App`が設定リポジトリを再読み込みし、全設定を取得します。不正な編集を検出した場合は最後に読み込めた設定を使います。
2. `SelectivePoller`が各設定をmode別に振り分け、並行して取得します。
3. `App`が取得テキストの前後空白を除き、空でなければSHA-256を計算します。
4. `DataRepository`が成功時のハッシュと確認時刻、または巡回失敗の連続数と直近エラーを保存します。成功時は失敗状態を消去します。
5. 内容変更、失敗状態への移行、失敗からの復旧をWebSocket通知に反映します。失敗通知は失敗状態へ移った時に一度だけ送ります。

各巡回サイクルには周期に基づく締切があり、取得に失敗した項目は締切までの間に同じサイクル内で最大3回試行します。締切後は新しい試行を開始せず、最終的に失敗した対象の状態をTOMLへ保存します。設定ファイルはサイクルごとに読み直し、正常に解析できた場合だけ現在の設定を置き換えます。設定・状態のTOML形式に後から追加した項目は既定値で読み込むため、既存ファイルも引き続き利用できます。

## 現在の境界と制約

- 保存先はローカルのTOMLファイルです。DBや外部通知サービスの実装はありません。
- WebSocket通知は変更・失敗・復旧のイベント配信です。接続時に`id`と`event`のクエリで通知を絞れます。履歴や初期状態をWebSocketでは配信しません。初期状態は`GET /api/v1/status`で取得できます。受信側がbroadcast容量100件分以上遅れると、その間の古いイベントを飛ばして配信を続けます。
- `GET /api/v1/status`は最新の巡回状態スナップショットを返します。`/ui`と配下のアセットはDioxus WebAssemblyバンドルを返し、画面は同じ状態・履歴APIとWebSocketを利用します。UIアセットはPatrolのビルド時に実行ファイルへ埋め込みます。状態APIとWebSocketに認証はありません。
- 内容が変わった時の新旧本文は`history.toml`へ別途保存し、`GET /api/v1/history`とWebUIから参照します。全体で既定100件（`--history-limit`で1〜1000件）、本文ごとに4 KiBまでに制限します。初回導入前の本文は保存されていません。
- `mode`を省略した設定はFullとして扱われます。
- Simpleモードは静的HTML取得向けで、ブラウザー上でのJavaScript実行はしません。
- Rust 2024 editionを使い、Rust 1.89以降のstable toolchainでビルドします。
