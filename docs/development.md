# 開発ガイド

## コードの置き場所

変更を加えるときは、責務に応じて次の場所を選びます。

| 変更内容 | 主な置き場所 |
| --- | --- |
| 値の意味、モデル、アプリが必要とする契約 | `src/domain/` |
| 巡回手順やpollerの選択・合成 | `src/application/` |
| TOML、HTTP、Playwrightなどの具体的な入出力 | `src/infrastructure/` |
| CLI、依存の組み立て、プロセス起動 | `src/main.rs` |
| 利用者・開発者向け資料 | `docs/`（概要と導線は`README.md`） |

具体的な外部実装をdomainに持ち込まず、applicationはdomainのtraitに依存させます。新しい保存先や取得方法を加える場合は、まず既存traitで表現できるか確認し、実装をinfrastructureに置いてください。

## 主要な契約

- `Poller::poll_multiple`は`(Id, Result<String, Error>)`のstreamを返します。完了順は入力順とは限りません。
- `Config`の`mode`は`Simple`か`Full`です。TOMLでは小文字表記を使い、省略時の既定値は`Full`です。
- `DataRepository::update`は内容が変わった場合に更新時刻を返し、同じ内容なら`None`を返します。
- `DataRepository::update_multiple`は複数のハッシュをまとめて保存します。変更時刻が必要な呼び出し側は`update_multiple_with_timestamps`を使います。既定実装は既存のtrait実装との互換性を保つため、更新前後の読み取りから変更時刻を求めます。TOML実装はまとめて保存し、ファイル書き込みを1回にします。
- `DataRepository::record_failure`は対象の連続失敗数と直近エラーを保存します。複数の失敗を処理する場合は`record_failures`を使います。成功時の`update`または`record_success`で失敗状態を消去します。
- `App`は空の取得内容を保存しません。抽出結果の正規化を変える場合は、既存の記録との比較結果にも影響することを考慮します。
- `App`は各巡回結果を`record_poll_results`でまとめて保存し、保存成功後に変更・失敗・復旧イベントを送ります。既定実装は既存の単件APIへ委譲し、TOML実装では巡回あたり最大1回のファイル書き込みです。保存失敗時はそれらのイベントを送りません。
- 空の取得結果は成功状態だけを記録し、`hash`と`last_checked`は更新しません。
- `TomlFileProxy::save`は同じディレクトリの一時ファイルに書いてから置き換えます。シリアライズや一時ファイルへの書き込みが失敗しても、既存ファイルを途中まで切り詰めません。
- アプリケーションからWebSocket中継タスクへの通知キューは容量128のbounded channelです。配信が遅れた場合は巡回処理が送信を待ち、キューが無制限に増えるのを防ぎます。
- `DataRepositoryActor`の要求キューは容量64のbounded channelです。キューが埋まると呼び出し側が送信を待ちます。このactorは現在の`main.rs`の起動経路では使用されていません。
- `PlaywrightPoller`の結果キュー容量はブラウザーページ数と同じです。結果の消費が遅いときは、並列巡回を続けずにキューの空きを待ちます。
- TOMLリポジトリはメモリ上のキャッシュを更新して保存します。データファイルは起動時に読み込まれ、稼働中の外部編集は自動再読込されません。
- 設定リポジトリは各巡回サイクルの開始時に再読込します。不正なファイルは最後に有効だった設定を維持してエラーを返します。

## ビルドと実行

nightly toolchainが必要です。

```sh
cargo +nightly build
cargo +nightly run -- --config-path ./config.example.toml --data-path ./data.toml --once
```

`config.example.toml`のURLとセレクターはサンプル値です。実際の対象サイトでの抽出結果やブラウザー動作を確認する場合は、用途に合う設定に置き換えてください。

## 資料の更新

利用者向けの挙動、引数、設定項目を変えた場合は`docs/usage.md`を更新してください。レイヤーや実行経路を変えた場合は`docs/architecture.md`と必要に応じて本書も更新します。READMEは概要とクイックスタートを保ち、詳細を重複して抱えないようにします。

ドキュメントはコードから確認できる仕様と、実行環境で検証した結果を区別して書いてください。未実行のブラウザー動作や外部サイトの状態を検証済みとして記載しないでください。
