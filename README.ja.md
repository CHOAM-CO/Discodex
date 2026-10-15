<p align="center"><img src="docs/assets/discodex-banner.png" alt="Discodex — テキストと音声での会話" width="960" /></p>
<p align="center"><strong>Discordで話し、同じ文脈を引き継ぎます。</strong><br />公式Codex app-serverの継続スレッドにテキストと音声を接続するRustブリッジです。</p>
<p align="center"><a href="README.md">English</a> · <a href="README.ko.md">한국어</a> · <a href="README.zh-CN.md">简体中文</a> · 日本語</p>
<p align="center"><a href="#はじめに">はじめに</a> · <a href="docs/installation.md">インストール</a> · <a href="docs/design.md">設計</a></p>

Discodexは、1つのDiscordアプリプロファイルを公式Codex app-serverの1つの継続スレッドに接続します。テキストと音声は同じ会話を使い、タスクの判断とツール実行はCodexが担当します。

## 主な機能

| 機能 | 説明 |
| --- | --- |
| 継続する文脈 | workerの再起動や音声通話の終了後も、同じCodex会話を続けられます。 |
| 音声と文字起こし | 許可された音声チャンネルで会話し、確定した文字起こしを通話ごとのDiscordスレッドに保存します。 |
| 質問と承認 | Codexからの質問や承認依頼をDiscordのフォームとボタンで受け取り、該当する依頼に回答します。 |
| エージェント設定 | プロファイル名とメモリ動作を設定し、`/discodex`から利用可能なモデル、推論の強さ、権限を選びます。 |
| Discordの情報と添付 | メッセージ、チャンネル、ユーザーを参照し、画像やファイルを会話に渡します。別チャンネルへの投稿は明示的な依頼がある場合に行います。 |

## はじめに

Rust 1.94以降、Cargo、および各OSのビルドツールが必要です。

```sh
git clone https://github.com/CHOAM-CO/Discodex.git
cd Discodex
./install
```

Windows PowerShellでは `./scripts/install.ps1` を実行します。インストーラーで English（既定）、한국어、简体中文、日本語を選べます。選んだ言語はインストールの案内、DiscodexのDiscord UI、エージェントの基本指示に適用されます。`DISCODEX_LANGUAGE=en`、`ko`、`zh`、`ja`をあらかじめ設定すると、言語を再度尋ねません。

インストーラーはCodexとDiscordを接続し、プロファイルとユーザー単位の自動起動を設定します。移行、更新、削除は[インストールガイド](docs/installation.md)を参照してください。

## 更新

追跡ファイルにローカル変更がないソース checkout から、既存の設定ファイルでプロファイルを更新します。

```sh
./update --config "<既存-config.json-のパス>"
```

Windows PowerShell：

```powershell
./scripts/update.ps1 -Config "<既存-config.json-のパス>"
```

追跡ファイルがクリーンな場合のみ checkout を fast-forward し、release バイナリをビルドして `--config` で指定したプロファイルに適用します。言語の再選択や設定の書き換えは行いません。選択済みの言語、資格情報、永続 Codex 会話状態はそのプロファイルに保持されます。製品所有バイナリの置換に失敗した場合は、以前のバイナリとサービス状態に戻します。`./install --update --config ...` と `./scripts/install.ps1 -Update -Config ...` も同じ更新処理を実行します。

## Discordコマンド

設定パネルには現在の値と、モデル、推論の強さ、Fastモード、権限プロファイル、入力の送信方法、音声の選択ボタンが表示されます。音声の変更は次の通話から適用されます。

- `/discodex settings`：ボタンで設定を選択
- `/discodex voice`：音声プロファイルを選択
- `/discodex status`：会話と設定を表示
- `/discodex models`：利用可能なモデルを表示
- `/discodex voice-join channel:<許可された音声チャンネル>`：音声チャンネルに参加
- `/discodex voice-leave`：音声通話を終了
- `/discodex interrupt`：現在の応答の停止を依頼
- `/discodex new`：新しい会話を開始

音声通話を終了しても継続会話は残ります。ドキュメント：[インストールと削除](docs/installation.md) · [製品設計](docs/design.md)
