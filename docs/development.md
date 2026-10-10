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
- `Timestamp::try_from_unix_secs`、`try_from_unix_millis`、`try_from_unix_nanos`は表現範囲外の値に`None`を返します。外部入力など範囲が保証されない値にはchecked形式を使います。`unix_nanos`は`i64`のナノ秒範囲外を上下限へ丸めます。
- `DataRepository::update`は内容が変わった場合に更新時刻を返し、同じ内容なら`None`を返します。
- `DataRepository::update_multiple`は複数のハッシュをまとめて保存します。変更時刻が必要な呼び出し側は`update_multiple_with_timestamps`を使います。既定実装は既存のtrait実装との互換性を保つため、更新前後の読み取りから変更時刻を求めます。TOML実装はまとめて保存し、ファイル書き込みを1回にします。
- `DataRepository::record_failure`は対象の連続失敗数と直近エラーを保存します。複数の失敗を処理する場合は`record_failures`を使います。成功時の`update`または`record_success`で失敗状態を消去します。
- `Config`は任意の`exclude_selectors`、`normalize_whitespace`、`poll_interval_minutes`を持ちます。TOMLでは省略可能で、除外・正規化を無効、巡回間隔を全体設定にする既定値を保ちます。
- `App`は空の取得内容を保存しません。抽出結果の正規化を変える場合は、既存の記録との比較結果にも影響することを考慮します。
- `App`は各巡回結果を`record_poll_results`でまとめて保存し、保存成功後に変更・失敗・復旧イベントを送ります。既定実装は既存の単件APIへ委譲し、TOML実装では巡回あたり最大1回のファイル書き込みです。保存失敗時はそれらのイベントを送りません。
- `App`は失敗した対象を同一サイクル内で最大3回試し、再試行の間に250ms、500msの指数バックオフを置きます。待ち時間も巡回締切に含め、締切を過ぎる場合は次の取得を開始しません。
- 空の取得結果は成功状態だけを記録し、`hash`と`last_checked`は更新しません。
- `TomlFileProxy::save`は同じディレクトリの一時ファイルに書いてから置き換えます。シリアライズや一時ファイルへの書き込みが失敗しても、既存ファイルを途中まで切り詰めません。
- アプリケーションからWebSocket中継タスクへの通知キューは容量128のbounded channelです。配信が遅れた場合は巡回処理が送信を待ち、キューが無制限に増えるのを防ぎます。
- WebSocketの`id`と`event`フィルターは各接続内で適用します。フィルターなしの既存接続は全イベントを受け取り、フィルター指定はbroadcast全体の配信には影響しません。
- `DataRepositoryActor`の要求キューは容量64のbounded channelです。キューが埋まると呼び出し側が送信を待ちます。このactorは現在の`main.rs`の起動経路では使用されていません。
- `PlaywrightPoller`のページ遷移とセレクター待機には各30秒のタイムアウトがあります。結果キュー容量はブラウザーページ数と同じです。結果の消費が遅いときは、並列巡回を続けずにキューの空きを待ちます。呼び出し側が結果ストリームを破棄した場合は、生成タスクも停止します。
- `HttpPoller`はHTTP応答本文を最大16 MiBまで読み込み、上限を超えた場合は本文の抽出を開始せずに失敗として返します。本文はchunk単位で読み、Content-Lengthが上限を超える場合は先に拒否します。
- `App::run_with_status`は巡回後の状態一覧を`watch`で公開します。HTTP status APIとWebUIはこのスナップショットを参照します。
- `App::run_with_history_and_shutdown`は終了要求を次の巡回開始前に確認します。巡回中に要求された場合は現在の結果保存まで完了し、その後は新しい巡回を開始しません。起動経路では通知中継タスクと履歴書き込みタスクの完了を先に待ち、WebSocketへcloseを送ってからWebサーバー終了を最大5秒待ちます。
- WebSocketのイベント送信中もshutdown watchを監視し、receiver作成時点ですでにshutdown済みの場合も現在値を確認します。送信待ちや遅れて接続したtaskが終了要求を見逃さないようにしてください。
- 通知中継taskまたは履歴書き込みtaskの予期しない終了は稼働中も監視します。異常を検知したら`App`へ終了要求を送り、残りの終了処理を完了してからエラーをプロセス終了結果へ伝えます。taskエラーで早期returnしてWebサーバーのshutdownを飛ばさないでください。
- 起動経路は`q`、標準入力EOF、Ctrl-Cを終了要求として扱い、Unix系OSではSIGTERMも捕捉します。HTTP・WebSocketの待受アドレスは`--web-listen`で指定し、`/healthz`はサーバー応答性のみを返します。
- 変更本文は`App::run_with_history`の任意チャンネルで巡回単位にまとめて保存層へ渡します。履歴は`TomlChangeHistoryRepository`が`history.toml`に別保存し、巡回中に複数の本文が変わってもTOMLへの書き込みは1回です。全体で既定100件（1〜1000件に設定可能）、本文ごとに4 KiBに制限します。本文を追加する場合は、履歴書き込みがAPIスナップショットに反映される流れと切り詰め表示も保ってください。
- 変更履歴を有効にした起動経路では、履歴ファイルの保存とAPIスナップショット更新が完了した後に変更通知を送ります。保存は最大3回試行し、すべて失敗した場合も変更通知自体は送ってエラーをログに残します。履歴リポジトリは書き込み失敗時にメモリー上のキャッシュを巻き戻します。
- 本文のハッシュは取得テキスト全体から計算します。履歴用のコピーだけは変更判定より前にUTF-8境界で4 KiBへ切り詰め、`content_truncated`で記録します。巡回中に取得本文すべての複製を保持しないよう、この上限を保ってください。
- 履歴書き込みタスクは終了時に送信側が閉じた後、キューに残った変更を保存してから終了します。プロセス終了経路を変更する場合は、このタスクの完了を待つ動作を維持してください。
- TOMLリポジトリはメモリ上のキャッシュを更新して保存します。データファイルは起動時に読み込まれ、稼働中の外部編集は自動再読込されません。
- 設定リポジトリは各巡回サイクルの開始時に再読込します。不正なファイルは最後に有効だった設定を維持してエラーを返します。

## ビルドと実行

UIの翻訳はui/locales/のFluent（FTL）カタログで管理します。メッセージIDはハイフン区切りにし、引数は`{ $name }`、複数形は数値セレクターで記述します。日本語・英語のカタログを更新したら両方のメッセージIDを揃え、UIソース、翻訳、またはCSSを変更した場合はPatrol本体の再ビルド前に`./scripts/build_ui.sh`を実行してください。

Rust 1.89以降のstable toolchainとDioxus CLI 0.7.10が必要です。UIを変更した場合は、Patrol本体のビルド前にDioxus UIを再バンドルしてください。

```sh
rustup target add wasm32-unknown-unknown
cargo install dioxus-cli --version 0.7.10 --locked
./scripts/build_ui.sh
cargo build
cargo run -- --config-path ./config.example.toml --data-path ./data.toml --once
```

フロントエンドは`ui/`に置き、`./scripts/build_ui.sh`が生成したWebアセットをルートの`build.rs`が本体へ埋め込みます。UIソースまたはCSSを変更したらこのスクリプトを実行してからPatrolを再ビルドします。生成物は`web/dist/`と`ui/target/`に置き、Gitへ含めません。UIをバンドルせずに本体だけを起動すると、`/ui`にはビルド手順を案内するページが表示されます。

`config.example.toml`のURLとセレクターはサンプル値です。実際の対象サイトでの抽出結果やブラウザー動作を確認する場合は、用途に合う設定に置き換えてください。

## テストと書式チェック

通常のテストとRustコードの書式チェックは次のコマンドで実行します。

```sh
cargo test
cargo fmt --check
```

Playwright依存crateはビルド時にブラウザードライバーを取得します。ブラウザーを起動しないチェックやユニットテストでは、環境変数を設定してドライバー取得を省略できます。

```sh
PLAYWRIGHT_SKIP_DRIVER_DOWNLOAD=1 cargo test
```

この環境変数を使った実行はRust側のチェック・ユニットテスト用です。Chromiumの起動、ページ遷移、実サイトからの抽出動作は検証しません。通常の実行ではビルド時にドライバーを取得し、Fullモードの初回起動時にChromiumをインストールします。

GitHub Actionsの`.github/workflows/ci.yml`は、pushとpull requestで書式チェック、Clippy、テスト、releaseビルドを実行します。CIではドライバー取得を省略するため、実ブラウザーの起動は確認しません。

## ベンチマーク

nightlyの`test` featureや`#[bench]`は使わず、`benches/manual.rs`を通常の実行ファイルとして実行します。`std::time::Instant`で計測し、`std::hint::black_box`で計算結果が最適化で除去されないようにします。

```sh
cargo bench --bench manual
```

SHA-256計算は64 B、4 KiB、1 MiBの入力を計測し、各サンプルでおよそ8 MiBを処理します。CSSセレクター解析は同じセレクターを各サンプルで10,000回解析します。各処理はウォームアップ後に5回計測し、中央値をns/opで表示します。比較時は同じマシン、同じRust toolchain、同じrelease設定で実行してください。これは専用の統計ベンチマークフレームワークではなく、処理時間を手早く比較するための目安です。

## 資料の更新

利用者向けの挙動、引数、設定項目を変えた場合は`docs/usage.md`を更新してください。レイヤーや実行経路を変えた場合は`docs/architecture.md`と必要に応じて本書も更新します。READMEは概要とクイックスタートを保ち、詳細を重複して抱えないようにします。

ドキュメントはコードから確認できる仕様と、実行環境で検証した結果を区別して書いてください。未実行のブラウザー動作や外部サイトの状態を検証済みとして記載しないでください。
