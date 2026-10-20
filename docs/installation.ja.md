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

保存済みのインストール一覧がない場合、`discodex install`は対話形式で作成します。`install-plan`は同じ一覧の作成だけを行います。インストーラーではプロファイル表示名、Codex実行環境（検出済みデスクトップアプリ/既存CLI、またはインストーラーが取得する公式Codex core）、Discordサーバー、ホームテキストチャンネル、文字起こしチャンネル、許可音声チャンネル、操作者ID、受信ポリシー、登録ユーザー・チャンネルID一覧、既存プロジェクトフォルダー、任意のモデルを指定します。

受信ポリシーは`Everyone`（全サーバーとDM）、`RegisteredServer`（設定したサーバー）、`RegisteredChannels`（登録チャンネルとそのスレッド）、`Paired`（登録ユーザーID）から選びます。これは入力を許可するユーザー範囲を決めます。許可リストと拒否リストはポリシーとは別に保持し、拒否リストを優先します。ポリシーが制御するのはメッセージ入力の範囲だけで、slash commandと管理権限には指定操作者の設定を使います。許可された入力はホームチャンネル、ボットへのメンション、登録チャンネルやそのスレッド、許可されたDMから既存のルーティング経路に入ります。メンションは許可判定を変えず、入力の振り分けだけに使います。新しいインストール計画で選んだポリシーとリストは、インストール時に実行時の`message-access.json`へ一度適用します。その後の通常の再インストールや更新では現在のポリシーとリストを保ちます。

Discord tokenは安全な入力欄で指定します。インストーラーはDiscordへの接続とnative接続を確認します。CodexログインやMFAが必要な場合は、公式ログイン手順をユーザーが完了します。workerの`config.json`に`effort`と`serviceTier`を指定すると、新しい永続スレッドに適用されます。`serviceTier: "off"`はnativeにtierの解除として渡します。省略時はnative既定値を使います。既存スレッドの再開時には`/discodex`で変更した現在のスレッド設定を上書きしません。

インストール一覧を保存しただけではサービスの準備完了ではありません。インストーラーはnativeアカウントとcatalog、Discord設定を確認し、ユーザーサービスを登録します。サービスが起動したことだけで、テキスト、質問・承認、音声の動作確認が済んだとは判断しません。

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
```

プレースホルダーのモデル名とendpointをproviderが対応する値に置き換えてください。ライブの`model/list` catalogを提供するのはアクティブなproviderだけです。ローカルの通常の`/v1/models`応答はCodexモデルmetadataに自動変換されません。Discodexの候補はアクティブなnative catalogと有効なworker設定のモデルを使い、既定、subagent、インストール時、現在のthreadのモデルを含みます。非アクティブなprofileファイルは自動列挙されません。

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

アクティブな公式catalogと登録済みprofileがモデル候補になります。モデル選択時は同じ永続threadでproviderを切り替え、履歴を引き継ぎます。同一provider内の変更はすぐ適用でき、別providerへの変更はthreadがidleになるまで待ちます。機能と推論強度はnative metadataが提供する場合にだけ表示されます。通常のローカル`/v1/models`応答はCodex model metadataに自動変換されません。[Codex provider設定ガイド](https://developers.openai.com/codex/config-advanced/)と[公式モデルmetadataスキーマ](https://github.com/openai/codex/blob/main/codex-rs/protocol/src/openai_models.rs)を参照してください。

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

更新スクリプトはGitHub公式Releaseから現在のプラットフォーム用のビルド済み実行ファイルとSHA-256チェックサムを取得して検証します。ソースcheckoutを変更したり、ソースからバイナリをビルドしたりしません。検証済みバイナリを`discodex update --source <new-binary> --config <existing-config>`に渡し、指定された所有プロファイルだけを更新します。`./install --update --config ...`とWindowsの`./scripts/install.ps1 -Update -Config ...`は同じ更新経路の別名です。

更新では既存プロファイルの言語、設定、認証情報、永続Codex会話状態を保ち、新しいプロファイルは作りません。バイナリ置換またはサービス再起動が失敗した場合は、以前の所有バイナリ、所有記録、サービス状態へロールバックします。ロールバックやデータ保全のため残された旧ディレクトリは自動削除しません。既存設定や会話状態を変更する作業は明示的なmigration手順で行い、更新では代用できません。

操作者専用の`/discodex update`は、6時間の確認間隔を待たずに最新の公開Releaseを即時確認します。固定済みの対象をすでに適用中なら、その対象を変更せず現在の状態を伝えます。それ以外はプロファイルの言語でUpdateまたはCancelの確認を表示します。Cancelは更新を開始せず、Updateは5分間の通知用idleタイマーを待たずにダウンロードを開始します。進行中のCodex呼び出し・応答、承認、または現在のメッセージ処理が終わってから適用し、待機中は状態を伝えます。再起動後、通知がミュート中または公開通知がなかった場合も、完了または失敗をホームチャンネルに別途投稿します。設定と永続Codexセッションを保ちます。

## 運用と削除

ユーザーサービスはLinuxの`systemd --user`、macOSログインユーザーの`~/Library/LaunchAgents`、Windows現在ユーザーのログオン時タスクとして登録されます。再インストールと削除では所有記録を確認し、この製品が所有するサービスとファイルだけを扱います。外部CodexのアプリやCLIが使われている場合、そのインストール、アカウント、会話、プロジェクトは保持します。

削除はインストール済みの既存バイナリから実行します。

```sh
discodex uninstall
```

所有サービスを停止し、所有する設定、Discord認証情報、登録コマンド、インストーラーが追加した構成だけを整理します。既存Codexログイン、スレッドの状態・履歴、プロジェクト、Discordサーバーの記録は保持します。

## Windowsの管理者権限と自動起動

WindowsインストーラーはプロファイルとCodexログインの準備を元のユーザー権限で行い、ログオン時タスクの登録時に一度UACを求めます。同じユーザーアカウントで管理者承認が必要です。別の管理者アカウントで認証するとタスクを変更せず停止します。UACをキャンセルした場合は準備済みファイルを残し、タスク登録を完了しません。

登録タスクは現在のユーザーがログインしたとき、Highest権限でworkerを起動します。workerと子Codexコマンドも管理者権限を使うため、通常のプログラム配置で追加昇格を求めません。Windows全体のUAC設定は変更しません。現在のアカウントは管理者である必要があり、ログアウト時にも動くSYSTEMサービスには切り替えません。

既存のLimitedタスクは、実行ファイル、設定パス、ユーザーSID、ログオントリガー、所有記録が一致する場合だけ切り替えます。登録に失敗した場合は以前のタスク定義と実行状態の復元を試みます。再インストールと削除でも所有記録の権限レベルを確認し、必要に応じてWindowsの承認を求めます。常駐タスクには実行時間制限を設けず、失敗時は1分間隔で最大3回再起動します。この再起動設定では、応答しないCodex turnまでは検出できません。

参考：[Windowsタスクのセキュリティコンテキスト](https://learn.microsoft.com/en-us/windows/win32/taskschd/security-contexts-for-running-tasks)、[UACと子プロセスの権限](https://learn.microsoft.com/en-us/windows/security/application-security/application-control/user-account-control/how-it-works)。

## macOSのフルディスクアクセス

macOSのフルディスクアクセス（FDA）はインストーラーが自動で確認・許可できません。実行ファイルのパスが変わる新規インストールや再インストールでは、署名identityの維持とは別にパス変更を検知します。所有権を確認した新しい実行ファイルの正確なパスを示し、Finderとシステム設定のFDA画面を開きます。この案内はnativeプロファイル設定とLaunchAgent起動の前に表示されます。権限が必要な場合は表示された実行ファイルを一覧に追加して有効にします。すでに許可済み、または不要ならEnterで続行し、`q`で中止します。続行してもmacOSがアクセスを許可したかは確認できません。対話端末がない場合はサービス起動前に停止し、実行ファイルの場所を案内します。同じ署名・同じパスでの通常更新ではFDA画面を再表示しません。Files & FoldersやAppleEventsの許可は別のOSダイアログで求められ、この案内が代わりに許可することはありません。

既存のad-hocインストールを`--migrate-adhoc`で切り替えるときも、新しい署名の実行ファイルを元のパスに置き換えた後、LaunchAgentの再起動前に同じ案内を開きます。設定画面を確認してEnterを押すと続行し、`q`を押すと以前のバイナリ、所有記録、サービス状態に戻します。

インストールまたは権限リセット後に案内を直接開くには、次を実行します。ownership記録にある実際の実行ファイルをFinderで選択し、システム設定のFDA画面を開きます。

```sh
discodex mac-permissions --config /path/to/profile/config.json
```

Finderで選択されたworkerを権限一覧に追加し、ユーザーが有効にしてください。このコマンドは許可を変更・確認しません。実際のインストールパスに必要な権限はOS設定で扱います。

## macOSの更新と権限の維持

macOSで初回インストールすると、このMacのログインキーチェーンにローカルコード署名鍵を一度作成します。インストール済みworkerと以降の更新には同じ証明書と固定識別子を使います。秘密鍵はキーチェーン内で書き出せない形で保存し、生成時の一時秘密鍵ファイルは削除します。鍵の作成と登録はインストーラーに含まれます。リモートインストールではログインユーザーのセッションで認証する一時LaunchAgentを使い、作業後に削除します。キーチェーンアクセスアプリを別途起動する必要はありません。macOSの認証画面が出たら許可してください。キーチェーンがロック中、または鍵使用を拒否した場合はファイル置換前に中止します。

署名メタデータと公開証明書は`~/Library/Application Support/Discodex/signing/`に保存します。旧`Codex Worker/signing/`ディレクトリがあり、新しい保存先がない場合はディレクトリ全体を原子的に移動して従来の署名identityを保ちます。署名identityを保っても、パス依存のTCC権限が新しい実行ファイルのパスに自動適用されるわけではありません。パスが変わるインストールでは上記FDA案内が新しいパスを示し、Files & FoldersとAppleEvents権限はmacOSが個別に尋ねる場合があります。新しい場所が空でも既存の署名identity/certificateが残っていれば、以前の署名保存先を使います。プロファイルを削除しても共有署名鍵とメタデータは再インストール用に保持します。このディレクトリやキーチェーン内の署名鍵を削除するとコードidentityを維持できないため、自動で新しい鍵を作りません。このローカル署名は公開配布用のDeveloper ID署名・公証の代わりではありません。

macOSでも共通の`./update --config "<既存config.jsonのパス>"`を使います。スクリプトはGitHub公式ReleaseのmacOS実行ファイルとSHA-256チェックサムを取得・検証してアプリの更新経路を呼び出します。既存のad-hoc署名を初めて移行するときだけ`--migrate-adhoc`を付けます。`./install --update --config ...`は同じ更新経路の旧別名です。更新では署名identityを維持し、選択したプロファイルのサービスだけを再起動します。もともと停止中のサービスは起動しません。設定、認証情報、Codex会話状態を保ち、失敗時は以前の実行ファイル、所有記録、サービス状態に戻します。

FDAが維持されているかはmacOS上でユーザーが確認します。製品はTCCデータベースを変更したり許可を自動化したりしません。Apple資料：[安定した署名とファイルアクセス権](https://developer.apple.com/forums/thread/678819)、[コード署名の同一性と自己署名](https://developer.apple.com/library/archive/technotes/tn2206/_index.html)。

## プロファイルの役割とメモリ

設定の`name`は表示名であり、会話の基本的なidentityです。workerは新規スレッドとサービス再起動後の既存スレッド再開に、この名前を持つAI同僚の基本promptを使います。音声Realtimeはテキストbacking modelの`realtimeStartInstructions`とは別のbackend promptを使います。workerは`experimental_realtime_ws_backend_prompt`を所有するCodex子プロセスの`--config`にだけ渡し、Codex全体設定ファイルは変更しません。公式selectorはこの設定を要求promptより優先し、両方がない場合は既定backend promptを選びます。現在のCodex既定backend promptは自分をCodexと名乗るため、名前付きのvoice identityには専用promptが必要です。接続時の自動挨拶は挿入せず、通話境界と既存会話の継続性を伝え、最初の発話を待ちます。役割指示を変えるために既存会話を削除したり新規スレッドを作ったりする必要はありません。[prompt selector](https://github.com/openai/codex/blob/740e5af33c71225640e0c1c1555c514c2c93ab74/codex-rs/core/src/realtime_prompt.rs)、[既定Realtime backend prompt](https://github.com/openai/codex/blob/740e5af33c71225640e0c1c1555c514c2c93ab74/codex-rs/prompts/templates/realtime/backend_prompt.md)。

`config.json`の任意項目`memoryEnabled: true`は、そのworkerで公式Codexメモリ機能、生成・利用、専用の参照・検索・ノート記録ツール、既存スレッドのメモリ生成を有効にします。`false`はそのworkerで無効にし、省略時は既存native設定を保ちます。再インストール時にもこの項目を保ちます。追加のインストール質問は設けず、他のCodexアプリの全体設定やアカウントファイルは変更しません。メモリ機能が有効でも、実際の抽出・統合はnativeの待機時間と使用条件に従います。

公式資料：[設定リファレンス](https://learn.chatgpt.com/docs/config-file/config-reference)、[既存スレッド再開の実装](https://github.com/openai/codex/blob/main/codex-rs/app-server/src/request_processors/thread_processor.rs)、[基本指示の選択優先順位](https://github.com/openai/codex/blob/main/codex-rs/core/src/session/mod.rs)。

## Discordからの更新

自動更新通知のRelease確認予定は`last_checked_unix`に保存します。初回確認だけ即時に行い、その後は保存済みの期限に従います。成功後は6時間、失敗後は15分で再確認し、起動時も保存済み予定を使います。確認ループは15秒ごとに動きます。自動通知はCodex接続中にアクティブturn、音声接続または終了処理、承認待ち、未処理入力、メッセージ処理、出力送信がなく、5分間idleの場合にホームチャンネルに表示します。すでに条件を満たしていれば追加待機はありません。

新しい公式Releaseの自動通知は、Codex接続中にアクティブturn、音声接続・終了処理、承認待ち、未処理入力、メッセージ処理、出力送信がなく5分間idleの場合にホームチャンネルへ表示します。Updateをクリックすると5分タイマーを待たずにダウンロードが始まります。進行中のCodex呼び出し・応答、承認、または現在のメッセージ処理が終わってから適用し、待機状態を案内します。再起動後、通知設定に関係なく完了または失敗をホームチャンネルへ別途投稿します。Skip this versionはそのバージョンだけを対象にし、Never notify againはプロファイルの通知を無効にします。`/discodex settings`から再び有効にできます。ボタン表示はプロファイルの言語に従います。

Discodexは承認済みGitHub Releaseから現在のOSとアーキテクチャ用のビルド済み実行ファイルを取得し、GitHub Release APIのSHA-256 digestとサイズを検証します。クライアントはソースを取得・ビルドしないため、Rust Cargoやプラットフォーム用ビルドツールは不要です。updaterは実行ファイルを置き換える前に進行中の処理終了を確認します。Releaseに現在のプラットフォーム用実行ファイルがない、またはdigest検証に失敗した場合は準備を中止して結果を伝えます。既存セッション、設定、自動起動、Macの署名identityを保ち、短時間再接続します。

通知対象はmainブランチの全commitではなく公式GitHub Releaseです。公開時にはCargoパッケージのバージョンとReleaseの`vX.Y.Z`タグを一致させます。手動更新スクリプトも同じプラットフォーム別Releaseバイナリを取得し、SHA-256チェックサムを確認します。

リリース担当者はバージョンのGit tagとdraft Releaseを作り、GitHub Actionsの`Build release binaries` workflowを`release_tag`とともに実行します。workflowはWindows、macOS、Linuxのx64/ARM64で単体テストとrelease buildを行い、6つの実行ファイル、SHA-256チェックサム、`discodex-installers.zip`をdraftに添付します。すべてのジョブと添付ファイルを確認してからReleaseを公開します。ソースアーカイブだけのReleaseは公開しません。

## 作業ツールの更新

更新後にworkerを再起動すると、既存Codexスレッドを保ったままnativeの`update_plan`、CLI作業ツール、管理ワークツリーツールが使えます。変更されたプランは新しいホームチャンネルEmbedカードで表示し、以前のカードは編集しません。同一イベントの再送は抑制し、プラン全体の文章はDiscordの上限を超える場合だけページ分割します。別途MCPをインストールしたり、ユーザー全体のCodex設定を編集したりする必要はありません。作業の閲覧・作成・分岐とワークツリー作成は同じ公式app-server接続を使います。新しい作業は呼び出し元のnative設定を引き継ぎ、既存会話やプロファイル設定を初期化しません。

## Native機能、プラグイン、運用ログ

Native app-server機能の設定はプロファイルごとの`native-features.json`に保存されます。CodeMode、host、multiagent v2と`wait_agent`、通常モードの質問、clock、token budget/history notes、非同期通知が含まれます。設定画面は実効native状態と保存値を分けて表示し、worker再起動が必要な変更を示します。

`/discodex plugins`でnativeプラグインカタログを参照し、Discordボタンから対応する読み取り・インストール・アンインストール操作を実行できます。推奨はnative elicitationを使います。プラグインの認証・承認、アカウントでの利用可否、entitlementは公式nativeフローで扱われ、アカウントとカタログに依存します。

非同期native質問にはDiscordのボタンまたは自由入力で回答でき、元の質問IDとnative threadとの関連を保持します。非同期通知は実行中turnを終了せずホームチャンネルに届きます。

メイン制御イベントキューは監視されたworkerが処理し、プロファイル単位のログを永続保存します。コンソールなしで起動したWindows GUI workerの診断にも利用できます。更新はビルド済みReleaseを使うため、クライアント端末でソースをビルドする必要はありません。
