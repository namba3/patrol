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

`DataRepositoryActor`はメッセージ経由でリポジトリ操作を直列化する補助実装です。現在の`main.rs`の起動経路では使用されていません。

### Infrastructure — `src/infrastructure/`

- `TomlConfigRepository`: 設定をTOMLから読み込み、変更をファイルに保存します。
- `TomlDataRepository`: ハッシュや時刻などの巡回状態をTOMLに保存します。
- `TomlFileProxy`: TOMLファイルをメモリ上のキャッシュと同期します。
- `HttpPoller`: HTTPで取得したHTMLをCSSセレクターで解析します。
- `PlaywrightPoller`: ヘッドレスChromiumでページを開き、DOM要素のテキストを取得します。

### Composition root — `src/main.rs`

CLI引数を解釈し、TOMLリポジトリと2種類のpollerを生成して`App`へ渡します。また、更新通知をWebSocketへ中継し、標準入力の`q`でプロセスを終了します。

## 巡回の流れ

1. `App`が設定リポジトリから全設定を読み込みます。
2. `SelectivePoller`が各設定をmode別に振り分け、並行して取得します。
3. `App`が取得テキストの前後空白を除き、空でなければSHA-256を計算します。
4. `DataRepository`がハッシュと確認時刻を保存します。前回と異なる場合は変更時刻も更新します。
5. 変更があった項目を端末一覧とWebSocket通知に反映します。

各巡回サイクルには周期に基づく締切があり、取得に失敗した項目は同じサイクル内で再試行対象になります。ポーリングエラーや保存エラーはログに出し、次の処理へ進みます。

## 現在の境界と制約

- 保存先はローカルのTOMLファイルです。DBや外部通知サービスの実装はありません。
- WebSocket通知は起動後の変更イベントのみです。履歴や初期状態を配信するAPIはありません。
- `mode`を省略した設定はFullとして扱われます。
- Simpleモードは静的HTML取得向けで、ブラウザー上でのJavaScript実行はしません。
- `src/lib.rs`がnightly featureを有効にしているため、nightly Rustが必要です。
