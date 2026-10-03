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
- `App`は空の取得内容を保存しません。抽出結果の正規化を変える場合は、既存の記録との比較結果にも影響することを考慮します。
- TOMLリポジトリは起動時にファイルを読み込み、メモリ上のキャッシュを更新して保存します。稼働中に外部編集したファイルは自動再読込されません。

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
