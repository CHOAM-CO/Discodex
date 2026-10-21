---
name: discodex
description: このエージェントを動かす Discord ブリッジ Discodex の使用、設定、更新、問題の診断を支援します。Discodex の設定、対応動作、メッセージやファイルの配信、音声接続、インストールに関する質問で使用します。
---

# Discodex

このガイドは Discodex **{{version}}** に含まれています。ブリッジについて回答するときは、インストール済みバージョンの参考文書と現在のツール結果を使ってください。

必要な文書の関連箇所だけを読んでください。

- [インストールと設定](references/installation.ja.md)：プロファイル設定、言語、プロバイダーとモデル、AGYのインストール・ログイン、承認設定、更新、プラットフォームのサービス、削除。
- [動作と設計](references/design.ja.md)：ホーム・送信元チャンネル・DMへの返信、添付と表、音声受信箱と文字起こし、継続する文脈、タスクツール、メモリー、更新処理。

Discodex は Discord とネイティブ Codex ランタイムを接続します。診断では Discord の配信、worker の動作、ネイティブモデルやツールの実行、プロバイダーの応答、ホスト OS の動作を区別してください。実際のエラーと現在の設定から問題の箇所を確認します。worker が動いているだけでは、配信や音声が正常だとは確認できません。

現在のツールスキーマと返されたカタログが、このセッションで使える機能を示します。変更時にはこの情報を使ってください。文書は手順を説明しますが、操作成功の証拠ではありません。実際に確認できた結果を伝えてください。

新しいバージョンは[公式リリース](https://github.com/CHOAM-CO/Discodex/releases)で確認してください。同梱の参考文書は、このバージョンの[設計](https://github.com/CHOAM-CO/Discodex/blob/v{{version}}/docs/design.ja.md)と[インストール](https://github.com/CHOAM-CO/Discodex/blob/v{{version}}/docs/installation.ja.md)文書です。他言語やリポジトリのページへの相対リンクは、これらの元ページから開けます。
