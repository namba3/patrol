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

`App`が巡回周期、設定取得、ポーリング、ハッシュ保存、更新一覧の出力を制御します。`SelectivePoller`は`Mode`に応じてFullまたはSimpleのpollerへ処理を振り分け、複数の取得ストリームをまとめます。

`DataRepositoryActor`は容量64のbounded channelを介してリポジトリ操作を直列化する補助実装です。キューが埋まると要求元が送信完了を待ちます。現在の`main.rs`の起動経路では使用されていません。

### Infrastructure — `src/infrastructure/`

- `TomlConfigRepository`: 設定をTOMLから読み込み、変更をファイルに保存します。
- `TomlDataRepository`: ハッシュや時刻などの巡回状態をTOMLに保存します。
- `TomlFileProxy`: TOMLファイルをメモリ上のキャッシュと同期します。
- `HttpPoller`: HTTPでHTMLを取得し、CSSセレクターによるCPU処理をブロッキング用スレッドプールで実行します。
- `PlaywrightPoller`: Fullモードの初回巡回時にPlaywrightとChromiumを初期化・準備し、DOM要素のテキストを取得します。ページ数を同時実行数と結果キュー容量の上限に使い、結果の消費が遅い場合は巡回側も待機します。Simpleモードだけを使う場合はブラウザーを起動・準備しません。

### Composition root — `src/main.rs`

CLI引数を解釈し、TOMLリポジトリと2種類のpollerを生成して`App`へ渡します。また、更新通知をWebSocketへ中継し、標準入力の`q`でプロセスを終了します。

## 巡回の流れ

1. `App`が設定リポジトリを再読み込みし、全設定を取得します。不正な編集を検出した場合は最後に読み込めた設定を使います。
2. `SelectivePoller`が各設定をmode別に振り分け、並行して取得します。
3. `App`が取得テキストの前後空白を除き、空でなければSHA-256を計算します。
4. `DataRepository`が成功時のハッシュと確認時刻、または巡回失敗の連続数と直近エラーを保存します。成功時は失敗状態を消去します。
5. 内容変更、失敗状態への移行、失敗からの復旧をWebSocket通知に反映します。失敗通知は失敗状態へ移った時に一度だけ送ります。

各巡回サイクルには周期に基づく締切があり、取得に失敗した項目は同じサイクル内で最大3回試行します。最終的に失敗した対象の状態はTOMLへ保存されます。設定ファイルはサイクルごとに読み直し、正常に解析できた場合だけ現在の設定を置き換えます。設定・状態のTOML形式に後から追加した項目は既定値で読み込むため、既存ファイルも引き続き利用できます。

## 現在の境界と制約

- 保存先はローカルのTOMLファイルです。DBや外部通知サービスの実装はありません。
- WebSocket通知は変更・失敗・復旧のイベント配信です。履歴や初期状態を配信するAPIはありません。受信側がbroadcast容量100件分以上遅れると、その間の古いイベントを飛ばして配信を続けます。
- `mode`を省略した設定はFullとして扱われます。
- Simpleモードは静的HTML取得向けで、ブラウザー上でのJavaScript実行はしません。
- `src/lib.rs`がnightly featureを有効にしているため、nightly Rustが必要です。
