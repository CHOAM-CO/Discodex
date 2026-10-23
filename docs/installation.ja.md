# インストールとアンインストール

<p align="center"><a href="installation.md">English</a> · <a href="installation.ko.md">한국어</a> · <a href="installation.zh-CN.md">简体中文</a> · 日本語</p>

このガイドではユーザー単位のインストールと更新を説明します。インストールスクリプトはGitHub公式ReleaseからOS・アーキテクチャに合うビルド済み実行ファイルとSHA-256チェックサムを取得して検証します。ローカルのRust/Cargoやプラットフォーム用ビルドツールは不要です。Linux向けビルドはUbuntu 22.04（glibc 2.35）で作成しており、これをLinux互換性の基準にしています。

Linux/macOS:

```sh
./install
```

Windows PowerShell:

```powershell
./scripts/install.ps1
```

## 言語の選択

開始時にEnglish（既定）、한국어、简体中文、日本語から選びます。選択した言語はインストール案内、DiscodexのDiscord UI、エージェントの基本指示に使われます。`DISCODEX_LANGUAGE=en`、`ko`、`zh`、`ja`を事前に設定するとbootstrapは言語を再確認せず、その値をインストーラーに渡します。

macOS/Linux:

```sh
DISCODEX_LANGUAGE=ja ./install
```

Windows PowerShell:

```powershell
$env:DISCODEX_LANGUAGE = 'ja'
./scripts/install.ps1
```

## 既存プロファイルの移行

既存のCodex Workerインストールからプロファイルを移す場合は、取得したDiscodex実行ファイルに対象の設定ファイルを指定してmigrationを実行します。Windowsでは実在する設定ファイルのパスをPowerShell形式で指定してください。

```sh
./discodex migrate --config "<既存config.jsonのパス>"
```

macOS、Linux、Windowsのmigrationは、指定プロファイルを所有記録とサービスで確認したうえで、設定、認証情報、インストール一覧、状態を新しい`discodex`のパスへコピーします。ロールバックとデータ保全のため元のパスは残します。移行後のプロファイルを確認するまでは元データを削除しないでください。

Linux/macOSでは`sh scripts/install.sh`を直接実行する方法もあります。bootstrapはGitHub Releaseのプラットフォーム別実行ファイルを取得してSHA-256チェックサムを検証し、`discodex`の名前とユーザーデータの既定パスを使います。対象ファイルがすでに存在する場合は上書きしません。

## 初回インストール

保存済みのインストール一覧がない場合、`discodex install`は対話形式で作成します。`install-plan`は同じ一覧の作成だけを行います。インストーラーではプロファイル表示名、Codexデータホーム（既存ホームの共有またはDiscodex専用ホーム）、Discordサーバー、ホームテキストチャンネル、文字起こしチャンネル、許可音声チャンネル、操作者ID、受信ポリシー、登録ユーザー・チャンネルID一覧、既存プロジェクトフォルダー、任意のモデルを指定します。

受信ポリシーは`Everyone`（全サーバーとDM）、`RegisteredServer`（設定したサーバー）、`RegisteredChannels`（登録チャンネルとそのスレッド）、`Paired`（登録ユーザーID）から選びます。これは入力を許可するユーザー範囲を決めます。許可リストと拒否リストはポリシーとは別に保持し、拒否リストを優先します。ポリシーが制御するのはメッセージ入力の範囲だけで、slash commandと管理権限には指定操作者の設定を使います。許可された入力はホームチャンネル、ボットへのメンション、登録チャンネルやそのスレッド、許可されたDMから既存のルーティング経路に入ります。メンションは許可判定を変えず、入力の振り分けだけに使います。新しいインストール計画で選んだポリシーとリストは、インストール時に実行時の`message-access.json`へ一度適用します。その後の通常の再インストールや更新では現在のポリシーとリストを保ちます。

ペア登録済みユーザーと許可リストのユーザーからのDMは、サーバー・チャンネルの受信ポリシーにかかわらずメンションなしで受け取ります。拒否リストと自身のメッセージの除外を優先し、`Everyone`では他のDM送信者も許可します。通常の自動出力、質問、承認、進捗はホームチャンネルのままです。リアルタイムで受信したDMへの回答は、`discord_send_message`に入力の`source.dm_id`を指定して元のDMに送ります。

Discord tokenは安全な入力欄で指定します。インストーラーはDiscordへの接続とnative接続を確認します。CodexログインやMFAが必要な場合は、公式ログイン手順をユーザーが完了します。workerの`config.json`に`effort`と`serviceTier`を指定すると、新しい永続スレッドに適用されます。`serviceTier: "off"`はnativeにtierの解除として渡します。省略時はnative既定値を使います。既存スレッドの再開時には`/discodex`で変更した現在のスレッド設定を上書きしません。

インストール一覧を保存しただけではサービスの準備完了ではありません。インストーラーはnativeアカウントとcatalog、Discord設定を確認し、ユーザーサービスを登録します。サービスが起動したことだけで、テキスト、質問・承認、音声の動作確認が済んだとは判断しません。

## 管理ランタイムとCodexデータ

Discodexは`third_party/codex`に固定したCodexフォークをビルド済みで配布します。既存デスクトップアプリやCLIのcoreは実行基盤として選択しません。公式パッケージ構造を使い、`bin/codex`、`bin/codex-code-mode-host`、`rg`と、OSに必要なLinuxの`bwrap`、Windowsのcommand-runner・sandbox-setup helperを含めます。Windows実行ファイルには`.exe`が付きます。

新規導入では既存Codexホームを共有し、ログイン・メモリ・設定・会話履歴を引き継げます。既存ホームを検出した場合は共有を推奨します。Discodexプロファイルデータ下の専用ホームも選べます。再インストールや更新では現在のホームを保持します。機能設定はDiscodexが保存して子プロセスやネイティブスレッドに渡し、共有`config.toml`を上書きしません。共有ホームの別々のセッションを利用でき、同じセッションは複数クライアントで同時に動かさないでください。

各Releaseは従来の`discodex-<target>[.exe]`と、`discodex-runtime-<target>.tar.gz`、`discodex-runtime-<target>.json`を提供します。マニフェストにはworker版、正確なフォークコミット、OS/アーキテクチャ、アーカイブと相対ファイルパスのチェックサムだけを記録し、ユーザープロファイル・アカウント・認証情報・ホストアドレス・ローカルインストールパスを含めません。更新は版別ランタイムを先に準備して所有するworker・設定・所有記録を切り替えます。ランタイム準備失敗時には応答が中断済みの場合があり、既存ファイルを保持して応答は自動再開しません。ネイティブデータホームはコピーも削除もしません。置換や再起動に失敗した場合は下記の順序で復元を試み、清掃時も未記録のユーザーファイルは残します。

本ガイドは現在のソースの動作を説明します。ダウンロード版は公開済みReleaseタグを確認してください。プラットフォームのビルド結果とユーザー検証結果は別途報告します。

プロファイルのプロジェクトには既定の `.agents/skills/discodex/` スキルを含めます。短い `SKILL.md` はプロファイルの言語を使い、同じビルドのインストール・設計文書を4言語で参照して、設定・更新・診断に必要な資料だけ読むよう案内します。インストール、更新の有効化、新しいネイティブ app-server 接続の準備時にヘルプを同期します。ユーザーが作成した同名スキルは保持し、管理スキル内でユーザーが編集したファイルも個別に保持します。同期エラーは警告として表示し、ボットの接続は止めません。基本プロンプトと共有 `CODEX_HOME` は変更しません。

## カスタムおよびローカルCodex provider

選択したCodexプロセスが実際に使うものと同じ `CODEX_HOME/config.toml` でproviderとモデルを設定します。Codexには組み込みの `ollama`、`lmstudio` providerがあり、Responses APIのカスタムproviderにも対応します。provider認証はCodexが処理します。`env_key` で指定した認証情報はworkerサービスの環境から利用できるようにしてください。秘密情報をDiscodexプロファイルファイルに保存しないでください。nativeの非OpenAI providerを使うだけでChatGPTアカウントやOpenAI API keyが必要になることはありません。

Ollamaではインストール済みモデル名を指定します。

```toml
model_provider = "ollama"
model = "example-local-model"
```

カスタムResponses API providerの例:

```toml
model_provider = "demo"
model = "example-model"

[model_providers.demo]
name = "Example provider"
base_url = "https://provider.example/v1"
env_key = "PROVIDER_API_KEY"
wire_api = "responses"
# 入力トークン数 API を提供するプロバイダーの場合だけ有効化:
# input_tokens_endpoint = "/responses/input_tokens"
```

プレースホルダーのモデル名とendpointをproviderが対応する値に置き換えてください。ライブの`model/list` catalogを提供するのはアクティブなproviderだけです。ネイティブの検出はCodexの `{ "models": [...] }` と互換形式の `{ "data": [...] }` の両方に対応します。互換項目からモデルIDと、`context_window`、`context_length`、`max_model_len`、`meta.n_ctx` で提供された文脈容量を読み取り、未指定の機能にはネイティブのモデル既定値を使います。すべてのプロバイダーが検出可能なカタログを提供するという意味ではありません。Discodexは有効なworker設定の既定、subagent、インストール時、現在のthreadのモデルも表示します。非アクティブなprofileファイルは自動列挙されません。

入力トークン数の計算を使うには、プロバイダー表の `input_tokens_endpoint` のコメントを外し、提供されているエンドポイントを設定してください。パスは `base_url` の末尾に追加され、上の `/responses/input_tokens` は `https://provider.example/v1/responses/input_tokens` になります。絶対URLで別のエンドポイントも指定できます。既存プロバイダーの認証と通信処理を使います。

プロバイダーは、生成と同じモデル入力・指示・ツールを受け取り、`{ "object": "response.input_tokens", "input_tokens": 123 }` を返す [OpenAI入力トークン数APIの契約](https://developers.openai.com/api/docs/guides/token-counting)を実装する必要があります。トークン化はプロバイダーが担当します。省略時はネイティブの使用量に基づく推定を維持し、プロバイダー名やモデル名からAPI対応を推測しません。

ライブの`model/list`結果はアクティブなnative catalogから取得します。Discodexはnative設定の`[profiles.NAME]`テーブルに`model`と`model_provider`を両方指定して明示登録したモデルも表示します。provider定義だけではモデルは登録されず、別の`*.config.toml` profileファイルも自動ロード・検索されません。例:

```toml
[model_providers.demo]
name = "Example provider"
base_url = "https://provider.example/v1"
env_key = "PROVIDER_API_KEY"
wire_api = "responses"

[profiles.example]
model = "example-model"
model_provider = "demo"
```

有効なnative catalogと登録したプロバイダー・モデルprofileから選択肢と機能メタデータを読みます。Codex内のプロバイダー変更ではnative遷移で履歴を引き継ぎます。Codex↔AGYは新しい作業セッションと過去履歴の参照を使います。通話中の変更では通話が元のセッション、以後の入力が新primaryを使います。[Codex設定ガイド](https://developers.openai.com/codex/config-advanced/)と[公式モデルmetadata](https://github.com/openai/codex/blob/main/codex-rs/protocol/src/openai_models.rs)を参照してください。

メイン・既定子モデルの選択では実行器・プロバイダー・モデルを一緒に保持し、子は親を切り替えず別の登録プロバイダーを利用できます。`[model_providers.<id>.capabilities]`の`encrypted_content`と`agent_messages`は独立した任意の真偽値です。省略時はネイティブ既定値に従い、OpenAI・Azure Responsesは暗号化履歴の再利用対応、ネイティブOpenAI分類のみが`AgentMessage`形式対応となります。明示値は実際のプロトコル対応に合わせてください。`encrypted_content = true`でもネイティブagent-message形式に非対応なら、その不透明な本文は通常のassistantメッセージ内で利用不可と明示します。原本の履歴は変更しません。[履歴の互換性](design.ja.md#プロバイダー切り替えと暗号化履歴)を参照してください。

## 承認方針、詳細設定とAntigravity

`/discodex settings`では**承認方針**と**権限プロファイル**を分けます。プロファイルの既定値は`on-request`で、`never`は明示的に選択する値です。保存済み方針と現在のスレッドの実効値を区別します。稼働中スレッドにはネイティブ設定APIを使い、開始・再開にも選択した方針を渡します。Windows管理者権限とモデル自身の安全動作は別の仕組みです。 承認の選択はプロファイルごとの`native-features.json`に保存し、共有Codex設定は上書きしません。

設定ホームの**既定サブエージェントモデル**は1か所だけで、**Antigravity**は独立した項目です。**詳細設定**にはnativeの機能スイッチをまとめます。nativeプロセス設定ではworker再起動の要否を示し、AGY設定は次のAGY実行から適用します。保存した子の既定値はスレッド作成・再開時に読み込み、既存の子は自身の設定を保ちます。

Antigravityは既定で無効です。インストーラーと**設定 → Antigravity**は公式ビルド済みCLIを検証し、プロファイル専用の場所へインストールします。外部AGYは再利用も上書きもしません。個人設定を読み取り専用の基準として取り込めます。Discodexの上書き値、MCP構成、`mcp(discodex_tasks/*)`許可ルールは常に専用ファイルに置き、個人設定と無関係なMCP項目は保持します。OSキーチェーンはAGY本来の動作に従い、削除時もAGYからログアウトしません。

準備時にアカウントと一覧を確認し、必要なら公式の対話ログインを起動します。実際の質問、ブラウザーリンク、要求された認証コード入力をDiscordまたはインストーラーに表示します。ブラウザー同意はユーザーが行います。キャンセル・終了で所有する準備プロセスを止め、完了済みの同意は取り消しません。サブスクリプションCLIを直接使い、SDKや非公開認証エンドポイントは使いません。

Antigravity設定ホームには有効化・準備、既存設定の利用、モデル一覧更新、nativeのツール・成果物・ターミナル方針、allow/ask/denyルールをまとめます。**すべての権限を許可**は公式auto-approve flagを渡す明示的な別設定で、画面を開くだけでは有効になりません。native方針は専用settings JSONへ保存し、次のAGY実行から適用します。全画面に終了、詳細画面には戻るもあります。

AGYモデル一覧は24時間キャッシュし、**設定 → Antigravity → モデル一覧を更新**で再取得できます。ベースモデルIDと`--effort`を分けて渡します。対応段階は取得したモデルメタデータを使い、選択した段階が未対応の場合は選択値以下の最も高い対応段階へ下げ、設定に実効値を示します。未知のモデルIDは保持し、CLIエラーを表示します。AGY側の強度を下げても、Codexの`ultra`のオーケストレーション指示は維持します。

AGY実行ごとにCLIプロセスを起動しますが、保存した正確な会話IDで再開し、新たに受理した入力と未送信の文字起こしだけを渡します。CodexとAGYを切り替えると新しい作業セッションになり、以前の記録は明示的に参照できます。子は既定で新しい文脈から開始します。対応するCodex子の明示的フォークは残し、未対応のAGYフォークは実際のエラーを返します。子のライフサイクルはMCP経由でCodexが管理します。

主実行器にかかわらず、音声は選択したCodexアカウントのOpenAI Realtime接続を使います。委任結果は現在の通話へ戻り、最後の委任後の発話は通話終了時に追加応答なしで保存し、次の通常要求へ渡します。通話中にprimaryセッションを変えても通話の作業セッションは維持します。CodexやDiscodexの失敗は、元のメッセージ・コード・状態とともに通知します。

AGYへのログインだけではOpenAI音声接続は認証されません。

## 更新

既存プロファイルの更新には、そのプロファイルの現在のconfigファイルを明示します。同じ端末に複数プロファイルがある場合も自動で選びません。更新では保存済み言語をそのまま使い、言語選択を再度求めません。

macOS/Linux:

```sh
./update --config "<既存config.jsonのパス>"
```

Windows PowerShell:

```powershell
./scripts/update.ps1 -Config "<既存config.jsonのパス>"
```

更新スクリプトはGitHub公式Releaseから現在のプラットフォーム用のビルド済み実行ファイルとSHA-256チェックサムを取得して検証します。ソースcheckoutを変更したり、ソースからバイナリをビルドしたりしません。検証済みバイナリを`discodex update --source <new-binary> --config <existing-config>`に渡します。新バージョンのupdaterが同じReleaseのCodexランタイムを準備してから、指定された所有プロファイルだけを更新します。`./install --update --config ...`とWindowsの`./scripts/install.ps1 -Update -Config ...`は同じ更新経路の別名です。

更新では既存プロファイルの言語、設定、認証情報、永続Codex会話状態を保ち、新しいプロファイルは作りません。置換や再起動に失敗した場合、以前のworker、プロファイル設定、ランタイム参照、所有記録、サービス状態の復元を試みます。必要な復元段階が失敗すると、その段階に依存する後続処理を止めて原因を報告し、新旧ファイルが混在した状態で再起動しません。未使用の準備済みランタイムは必要なサービス復元の後に削除し、削除の失敗も報告します。ロールバックやデータ保全のため残された旧ディレクトリは自動削除しません。新しい worker は起動準備の完了と更新完了の通知前に、管理設定を現在の規則に合わせて再適用します。設定済みの AGY は無効な状態でも、専用 MCP 登録・許可ルール・hook を更新します。Codex に登録した Discodex 専用の `discodex_tasks` サーバーではツールを自動承認します。個人設定、他の MCP サーバー、保存済みの選択値は保持します。設定の適用に失敗した場合はホームチャンネルに原因を通知し、通常の起動を完了しません。

操作者専用の`/discodex update`は、6時間の確認間隔を待たずに最新の公開Releaseを即時確認します。固定済み対象をすでに適用中なら、その対象を変更せず現在の状態を伝えます。それ以外はプロファイルの言語で「更新」または「キャンセル」の確認画面を表示し、「更新」を選ぶとsubagent作業を含む進行中の作業を中断して接続中の音声通話を終了すると明示します。「キャンセル」では現在の作業に触れません。「更新」を選ぶとまずReleaseをダウンロードして検証します。worker取得・検証に失敗した場合、現在の作業はそのまま続きます。準備成功後に限り、進行中の作業を中断し、音声通話を終了して直ちに再起動し、適用します。再起動後、通知設定に関係なく完了または失敗をホームチャンネルに別途投稿します。会話履歴と設定を保ち、中断した応答は自動再開せず、再開promptや空入力も送りません。

## 運用と削除

ユーザーサービスはLinuxの`systemd --user`、macOSログインユーザーの`~/Library/LaunchAgents`、Windows現在ユーザーのログオン時タスクとして登録されます。再インストールと削除では所有記録を確認し、この製品が所有するサービスとファイルだけを扱います。外部CodexのアプリやCLIが使われている場合、そのインストール、アカウント、会話、プロジェクトは保持します。

削除はインストール済みの既存バイナリから実行します。

```sh
discodex uninstall
```

所有サービスを停止し、所有する設定、Discord認証情報、登録コマンド、インストーラーが追加した構成だけを整理します。既存Codexログイン、スレッドの状態・履歴、プロジェクト、Discordサーバーの記録は保持します。

このプロファイルが所有するAGY実行ファイルも削除します。外部AGY、共有設定、OSのアカウントキーチェーンは保持し、AGYからログアウトしません。

## Windowsの管理者権限と自動起動

WindowsインストーラーはプロファイルとCodexログインの準備を元のユーザー権限で行い、ログオン時タスクの登録時に一度UACを求めます。同じユーザーアカウントで管理者承認が必要です。別の管理者アカウントで認証するとタスクを変更せず停止します。UACをキャンセルした場合は準備済みファイルを残し、タスク登録を完了しません。

登録タスクは現在のユーザーがログインしたとき、Highest権限でworkerを起動します。workerと子Codexコマンドも管理者権限を使うため、通常のプログラム配置で追加昇格を求めません。Windows全体のUAC設定は変更しません。現在のアカウントは管理者である必要があり、ログアウト時にも動くSYSTEMサービスには切り替えません。

既存のLimitedタスクは、実行ファイル、設定パス、ユーザーSID、ログオントリガー、所有記録が一致する場合だけ切り替えます。登録に失敗した場合は以前のタスク定義と実行状態の復元を試みます。再インストールと削除でも所有記録の権限レベルを確認し、必要に応じてWindowsの承認を求めます。常駐タスクには実行時間制限を設けず、失敗時は1分間隔で最大3回再起動します。この再起動設定では、応答しないCodex turnまでは検出できません。

参考：[Windowsタスクのセキュリティコンテキスト](https://learn.microsoft.com/en-us/windows/win32/taskschd/security-contexts-for-running-tasks)、[UACと子プロセスの権限](https://learn.microsoft.com/en-us/windows/security/application-security/application-control/user-account-control/how-it-works)。

## macOSのフルディスクアクセス

macOSのフルディスクアクセス（FDA）はインストーラーが自動で確認・許可できません。実行ファイルのパスが変わる新規インストールや再インストールでは、署名identityの維持とは別にパス変更を検知します。所有権を確認した新しい実行ファイルの正確なパスを示し、Finderとシステム設定のFDA画面を開きます。この案内はnativeプロファイル設定とLaunchAgent起動の前に表示されます。権限が必要な場合は表示された実行ファイルを一覧に追加して有効にします。すでに許可済み、または不要ならEnterで続行し、`q`で中止します。続行してもmacOSがアクセスを許可したかは確認できません。対話端末がない場合はサービス起動前に停止し、実行ファイルの場所を案内します。同じ署名・同じパスでの通常更新ではFDA画面を再表示しません。Files & FoldersやAppleEventsの許可は別のOSダイアログで求められ、この案内が代わりに許可することはありません。

既存のad-hocインストールを`--migrate-adhoc`で切り替えるときも、新しい署名の実行ファイルを元のパスに置き換えた後、LaunchAgentの再起動前に同じ案内を開きます。設定画面を確認してEnterを押すと続行し、`q`を押すと中止し、上記の復元順序に従って以前のバイナリ、所有記録、サービス状態の復元を試みます。

インストールまたは権限リセット後に案内を直接開くには、次を実行します。ownership記録にある実際の実行ファイルをFinderで選択し、システム設定のFDA画面を開きます。

```sh
discodex mac-permissions --config /path/to/profile/config.json
```

Finderで選択されたworkerを権限一覧に追加し、ユーザーが有効にしてください。このコマンドは許可を変更・確認しません。実際のインストールパスに必要な権限はOS設定で扱います。

## macOSの更新と権限の維持

macOSで初回インストールすると、このMacのログインキーチェーンにローカルコード署名鍵を一度作成します。インストール済みworkerと以降の更新には同じ証明書と固定識別子を使います。秘密鍵はキーチェーン内で書き出せない形で保存し、生成時の一時秘密鍵ファイルは削除します。鍵の作成と登録はインストーラーに含まれます。リモートインストールではログインユーザーのセッションで認証する一時LaunchAgentを使い、作業後に削除します。キーチェーンアクセスアプリを別途起動する必要はありません。macOSの認証画面が出たら許可してください。キーチェーンがロック中、または鍵使用を拒否した場合はファイル置換前に中止します。

署名メタデータと公開証明書は`~/Library/Application Support/Discodex/signing/`に保存します。旧`Codex Worker/signing/`ディレクトリがあり、新しい保存先がない場合はディレクトリ全体を原子的に移動して従来の署名identityを保ちます。署名identityを保っても、パス依存のTCC権限が新しい実行ファイルのパスに自動適用されるわけではありません。パスが変わるインストールでは上記FDA案内が新しいパスを示し、Files & FoldersとAppleEvents権限はmacOSが個別に尋ねる場合があります。新しい場所が空でも既存の署名identity/certificateが残っていれば、以前の署名保存先を使います。プロファイルを削除しても共有署名鍵とメタデータは再インストール用に保持します。このディレクトリやキーチェーン内の署名鍵を削除するとコードidentityを維持できないため、自動で新しい鍵を作りません。このローカル署名は公開配布用のDeveloper ID署名・公証の代わりではありません。

macOSでも共通の`./update --config "<既存config.jsonのパス>"`を使います。スクリプトはGitHub公式ReleaseのmacOS実行ファイルとSHA-256チェックサムを取得・検証してアプリの更新経路を呼び出します。既存のad-hoc署名を初めて移行するときだけ`--migrate-adhoc`を付けます。`./install --update --config ...`は同じ更新経路の旧別名です。更新では署名identityを維持し、選択したプロファイルのサービスだけを再起動します。もともと停止中のサービスは起動しません。設定、認証情報、Codex会話状態を保ち、失敗時は上記の順序で以前のインストール構成とサービス状態の復元を試みます。

FDAが維持されているかはmacOS上でユーザーが確認します。製品はTCCデータベースを変更したり許可を自動化したりしません。Apple資料：[安定した署名とファイルアクセス権](https://developer.apple.com/forums/thread/678819)、[コード署名の同一性と自己署名](https://developer.apple.com/library/archive/technotes/tn2206/_index.html)。

## プロファイルの役割とメモリ

設定の`name`は表示名であり、会話の基本的なidentityです。workerは新規スレッドとサービス再起動後の既存スレッド再開に、この名前を持つAI同僚の基本promptを使います。音声Realtimeはテキストbacking modelの`realtimeStartInstructions`とは別のbackend promptを使います。workerは`experimental_realtime_ws_backend_prompt`を所有するCodex子プロセスの`--config`にだけ渡し、Codex全体設定ファイルは変更しません。公式selectorはこの設定を要求promptより優先し、両方がない場合は既定backend promptを選びます。現在のCodex既定backend promptは自分をCodexと名乗るため、名前付きのvoice identityには専用promptが必要です。接続時の自動挨拶は挿入せず、通話境界と既存会話の継続性を伝え、最初の発話を待ちます。役割指示を変えるために既存会話を削除したり新規スレッドを作ったりする必要はありません。[prompt selector](https://github.com/CHOAM-CO/codex/blob/83086188ae0614bd882818ff1aa23f3194962012/codex-rs/core/src/realtime_prompt.rs#L5-L24)、[既定Realtime backend prompt](https://github.com/CHOAM-CO/codex/blob/83086188ae0614bd882818ff1aa23f3194962012/codex-rs/prompts/templates/realtime/backend_prompt.md#L1-L3)。

`config.json`の任意項目`memoryEnabled: true`は、そのworkerで公式Codexメモリ機能、生成・利用、専用の参照・検索・ノート記録ツール、既存スレッドのメモリ生成を有効にします。`false`はそのworkerで無効にし、省略時は既存native設定を保ちます。再インストール時にもこの項目を保ちます。追加のインストール質問は設けず、他のCodexアプリの全体設定やアカウントファイルは変更しません。メモリ機能が有効でも、実際の抽出・統合はnativeの待機時間と使用条件に従います。

公式資料：[設定リファレンス](https://learn.chatgpt.com/docs/config-file/config-reference)、[既存スレッド再開の実装](https://github.com/openai/codex/blob/main/codex-rs/app-server/src/request_processors/thread_processor.rs)、[基本指示の選択優先順位](https://github.com/openai/codex/blob/main/codex-rs/core/src/session/mod.rs)。

## Discordからの更新

自動更新通知のRelease確認予定は`last_checked_unix`に保存します。初回確認だけ即時に行い、その後は保存済みの期限に従います。成功後は6時間、失敗後は15分で再確認し、起動時も保存済み予定を使います。確認ループは15秒ごとに動きます。自動通知はCodex接続中にアクティブturn、音声接続または終了処理、承認待ち、未処理入力、メッセージ処理、出力送信がなく、5分間idleの場合にホームチャンネルに表示します。すでに条件を満たしていれば追加待機はありません。

新しい公式Releaseの自動通知は、Codex接続中にアクティブturn、音声接続・終了処理、承認待ち、未処理入力、メッセージ処理、出力送信がなく5分間idleの場合にホームチャンネルへ表示します。「更新」をクリックすると先にReleaseをダウンロードして検証します。worker取得・検証に失敗した場合、現在の作業は続きます。準備成功後に進行中の作業（subagent作業を含む）を中断し、接続中の音声通話を終了して5分タイマーを待たずに直ちに再起動・適用します。再起動後、通知設定に関係なく完了または失敗をホームチャンネルへ別途投稿します。会話履歴と設定を保ち、中断した応答は自動再開せず、再開promptや空入力も送りません。Skip this versionはそのバージョンだけを対象にし、Never notify againはプロファイルの通知を無効にします。`/discodex settings`から再び有効にできます。ボタン表示はプロファイルの言語に従います。

Discodexは承認済みGitHub Releaseから現在のOSとアーキテクチャ用のビルド済み実行ファイルを取得し、GitHub Release APIのSHA-256 digestとサイズを検証します。クライアントはソースを取得・ビルドしないため、Rust Cargoやプラットフォーム用ビルドツールは不要です。準備が成功した後にupdaterが実行ファイルを置き換えてサービスを直ちに再起動します。ダウンロードまたは検証に失敗した場合、現在の実行ファイルとworkerはそのまま稼働し、結果を伝えます。成功した更新では会話履歴、設定、自動起動、Macの署名identityを保ち、短時間再接続します。

通知対象はmainブランチの全commitではなく公式GitHub Releaseです。公開時にはCargoパッケージのバージョンとReleaseの`vX.Y.Z`タグを一致させます。手動更新スクリプトも同じプラットフォーム別Releaseバイナリを取得し、SHA-256チェックサムを確認します。

リリース担当者はバージョンのGit tagとdraft Releaseを作り、GitHub Actionsの`Build release binaries` workflowを`release_tag`とともに実行します。workflowはWindows、macOS、Linuxのx64/ARM64で単体テストとrelease buildを行い、6つのworker実行ファイル、SHA-256チェックサム、プラットフォーム別Codexランタイムアーカイブとマニフェスト、`discodex-installers.zip`をdraftに添付します。すべてのジョブと添付ファイルを確認してからReleaseを公開します。ソースアーカイブだけのReleaseは公開しません。

## 作業ツールの更新

更新後にworkerを再起動すると、既存Codexスレッドを保ったままnativeの`update_plan`、CLI作業ツール、管理ワークツリーツールが使えます。変更されたプランは新しいホームチャンネルEmbedカードで表示し、以前のカードは編集しません。同一イベントの再送は抑制し、プラン全体の文章はDiscordの上限を超える場合だけページ分割します。別途MCPをインストールしたり、ユーザー全体のCodex設定を編集したりする必要はありません。作業の閲覧・作成・分岐とワークツリー作成は同じ公式app-server接続を使います。新しい作業は呼び出し元のnative設定を引き継ぎ、既存会話やプロファイル設定を初期化しません。

## 詳細設定、プラグイン、運用ログ

Native app-server機能の設定はプロファイルごとの`native-features.json`に保存されます。CodeMode、host、multiagent v2と`wait_agent`、通常モードの質問、clock、token budget/history notes、非同期通知が含まれます。設定画面は実効native状態と保存値を分けて表示し、worker再起動が必要な変更を示します。

`/discodex plugins`でnativeプラグインカタログを参照し、Discordボタンから対応する読み取り・インストール・アンインストール操作を実行できます。推奨はnative elicitationを使います。プラグインの認証・承認、アカウントでの利用可否、entitlementは公式nativeフローで扱われ、アカウントとカタログに依存します。

非同期native質問にはDiscordのボタンまたは自由入力で回答でき、元の質問IDとnative threadとの関連を保持します。非同期通知は実行中turnを終了せずホームチャンネルに届きます。

メイン制御イベントキューは監視されたworkerが処理し、プロファイル単位のログを永続保存します。コンソールなしで起動したWindows GUI workerの診断にも利用できます。更新はビルド済みReleaseを使うため、クライアント端末でソースをビルドする必要はありません。
