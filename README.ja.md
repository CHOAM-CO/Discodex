<p align="center"><img src="docs/assets/discodex-banner.png" alt="Discodex — テキストと音声での会話" width="960" /></p>
<p align="center"><strong>Discordで話し、同じ文脈を引き継ぎます。</strong><br />公式Codex app-serverの永続スレッドにテキストと音声をつなぐRustブリッジです。</p>
<p align="center"><a href="README.md">English</a> · <a href="README.ko.md">한국어</a> · <a href="README.zh-CN.md">简体中文</a> · 日本語</p>
<p align="center"><a href="#はじめに">はじめに</a> · <a href="docs/installation.ja.md">インストール</a> · <a href="docs/design.ja.md">設計</a></p>

Discodexは1つのDiscordアプリプロファイルを公式Codex app-serverの1つの永続スレッドに接続します。テキストと音声は同じ会話を使い、タスクの判断とツール実行はCodexが行います。

## 主な機能

| 機能 | 説明 |
| --- | --- |
| 継続する文脈 | worker再起動後も同じCodex会話を続けられます。通話終了後も会話は残ります。 |
| 音声と文字起こし | 許可された音声チャンネルで会話し、確定した文字起こしを通話ごとのDiscordスレッドに記録します。 |
| 質問と承認 | Codexの質問や承認要求にDiscordフォームとボタンで答え、元の要求に応答します。 |
| エージェント設定 | プロファイル名とメモリ動作を設定し、`/discodex settings`から利用可能なモデル、推論強度、権限、入力方式を選べます。 |
| メッセージ受信範囲 | `Everyone`、`RegisteredServer`、`RegisteredChannels`、`Paired`から選び、許可リストと拒否リストを管理できます。拒否リストが優先されます。 |
| Markdown表 | 完了した通常回答と`discord_send_message`の表を折り返し・ページ分割したPNG添付で表示し、インラインの太字・斜体・下線と前後の文章を保持します。ローカライズされた**原文を表示**ボタンを押すと、押したユーザーだけがMarkdown原文をページごとに確認・コピーできます。 |
| 作業の進捗 | 変更されたプランごとにホームチャンネルへ新しいEmbedカードを作り、以前のカードは編集しません。完了数/総数、完了チェックと取り消し線、進行中の矢印、未着手マーカー、状態色を表示し、同一イベントの再送は抑制し、プラン全文はDiscordの実際の上限に合わせてページ分割し、切り詰めません。CLI作業・管理ワークツリー機能もDiscordに接続します。 |
| Discordの情報と添付 | メッセージ、チャンネル、ユーザーを参照し、画像やファイルを会話に渡します。別チャンネルへの投稿は明示的な依頼に従います。 |
| Native機能 | プロファイルごとの`native-features.json`で公式app-server機能を管理します。設定画面は実効状態と保存値を区別し、再起動が必要な変更を示します。 |
| プラグイン | `/discodex plugins`で現在のnativeカタログを確認し、Discordボタンで利用可能な読み取り・インストール・アンインストールを行います。推奨と認証は公式nativeフローに従います。 |
| 非同期の質問と通知 | nativeの質問にDiscordボタンまたは自由入力で回答でき、元の質問IDとthreadに結び付きます。非同期通知は進行中turnを終了せずホームチャンネルに届きます。 |
| イベント処理とログ | メイン制御イベントは監督対象のconsumerが処理し、プロファイル別の永続ログを保存します。コンソールなしで起動するWindows GUI workerの調査にも使えます。更新はビルド済みReleaseを使います。 |

通話中、ホームチャンネルへの入力や他チャンネルでのボットメンションは受信箱に保管します。通知には投稿者、チャンネル、本文の冒頭、添付の有無を表示します。プレビューは全文取得を意味しません。音声モデルは必要な場合、または依頼された場合だけ受信箱を読みます。通話後も未処理項目は保持されます。

受信ポリシーは入力を許可するユーザー範囲を決めます。許可された入力は、ホームチャンネル、ボットへのメンション、登録チャンネルやそのスレッド、許可されたDMから既存のルーティング経路に入ります。メンション自体はポリシーを上書きしません。

通常の回答、質問、承認、進捗はホームチャンネルに届きます。ホームチャンネルの入力には元投稿への返信を付け、回答では入力投稿者への明示的なユーザーメンションを使用できます。別チャンネルに投稿するのは、宛先と送信をユーザーが明示した場合だけです。

## はじめに

[最新Release](https://github.com/CHOAM-CO/Discodex/releases/latest)から`discodex-installers.zip`をダウンロードして展開し、そのフォルダーで実行します。ReleaseにはWindows、macOS、Linux向けx64/ARM64のビルド済み実行ファイルが含まれます。スクリプトがOSとアーキテクチャに合うファイルを取得するため、クライアントにRust、Cargo、Gitは不要です。

```sh
./install
```

Windows PowerShellでは`./scripts/install.ps1`を実行します。インストーラーでEnglish（既定）、한국어、简体中文、日本語を選べます。選んだ言語はインストール案内、Discord UI、エージェントの基本指示に使われます。`DISCODEX_LANGUAGE=en`、`ko`、`zh`、`ja`を事前に設定すると、言語を再度尋ねません。

インストーラーはCodexとDiscordを接続し、プロファイルとユーザー単位の自動起動を設定します。移行、更新、削除の詳細は[日本語インストールガイド](docs/installation.ja.md)を参照してください。

## カスタムモデルとローカルモデル

メインとsubagentの選択画面ではアクティブproviderのcatalogとworkerの有効設定にあるモデルを使います。既定、subagent、インストール時、現在のthreadのモデルが含まれます。**既定subagentモデル**はDiscodex所有の上書きとして保存され、worker再起動後に有効になります。設定例と検出方法は[カスタムおよびローカルCodex provider](docs/installation.ja.md#カスタムおよびローカルcodex-provider)を参照してください。

## 更新

インストーラーバンドルの更新スクリプトに既存プロファイルの設定ファイルを指定します。

```sh
./update --config "<既存config.jsonのパス>"
```

Windows PowerShell:

```powershell
./scripts/update.ps1 -Config "<既存config.jsonのパス>"
```

更新スクリプトは対象プラットフォーム用の公式Releaseバイナリを取得・検証し、指定したプロファイルに適用します。DiscordのRelease確認は保存した`last_checked_unix`に基づきます。初回だけ即時に確認し、その後は確認期限が来たときに行います。通常の次回確認は6時間後、失敗後は15分後です。起動時にも期限を確認しますが、毎回強制的に確認することはありません。期限確認ループは15秒間隔で動きます。新しいReleaseの通知タイミングはRelease確認とは独立しており、Codex接続中にturn、音声接続・終了処理、承認待ち、未処理入力、メッセージ処理、出力送信がなく5分間アイドルならホームチャンネルへ通知します。Releaseが見つかった時点ですでに条件を満たしていれば、さらに5分待ちません。Discordの更新通知も同じビルド済みバイナリを使います。保存済み言語、設定、認証情報、永続Codex会話状態を保ちます。置換に失敗した場合は以前のバイナリとサービス状態に戻します。`./install --update --config ...`と`./scripts/install.ps1 -Update -Config ...`も同じ更新経路を使います。v0.1.3より前のプロファイルでは、インストーラーバンドルから一度手動更新してself-updaterを導入してください。この更新は既存セッションと設定を保持し、その後はDiscordの通知から更新できます。

`/discodex update`はqueued、preparing、ready、applyingの対象があればそのまま保持します。対象がない場合に限り、6時間の確認間隔を待たずに最新Releaseを確認し、通知ボタンと同じ検証済みupdaterによる適用をキューに入れます。Codex turnを作らず、作業や音声を中断せず、通常の5分間のidle条件を待ちます。通知がミュート中、またはそのバージョンをスキップ済みでも実行でき、設定は変更しません。対象は固定したまま保持し、失敗した固定対象は新たなGitHub確認なしで再試行します。最新状態、更新なし、プラットフォーム用assetなし、エラーを結果として伝えます。

## Discordコマンド

設定パネルは現在値を表示し、モデル、推論強度、サービス階層、権限プロファイル、入力の送信方法、音声を選べます。音声の変更は次の通話から反映されます。

- `/discodex settings`：ボタンで設定を選ぶ
- `/discodex voice`：音声プロファイルを選ぶ
- `/discodex status`：会話と設定を表示する
- `/discodex voice-join channel:<許可された音声チャンネル>`：音声チャンネルに参加する
- `/discodex voice-leave`：通話を終了する
- `/discodex interrupt`：現在の応答の停止を依頼する
- `/discodex update`：更新を確認して適用する
- `/discodex new`：新しい会話を開始する

音声通話を終了しても永続会話は残ります。ドキュメント：[日本語インストールガイド](docs/installation.ja.md) · [日本語の製品設計](docs/design.ja.md)
