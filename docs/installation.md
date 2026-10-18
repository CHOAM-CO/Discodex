# Installation and removal

Download `discodex-installers.zip` from the [latest GitHub release](https://github.com/CHOAM-CO/Discodex/releases/latest), extract it, and open the extracted directory in a terminal. Discodex installs for the current user. The installer downloads a prebuilt executable for the detected OS and architecture from the official GitHub Release and verifies its SHA-256 digest. Client machines do not need Rust, Cargo, Git, or platform build tools. Linux release binaries are built on Ubuntu 22.04 (glibc 2.35), which is the Linux compatibility baseline.

<p align="center">English · <a href="installation.ko.md">한국어</a> · <a href="installation.zh-CN.md">简体中文</a> · <a href="installation.ja.md">日本語</a></p>

On Linux or macOS:

```sh
./install
```

On Windows PowerShell:

```powershell
./scripts/install.ps1
```

## Language

The installer offers English (default), Korean, Simplified Chinese, and Japanese. The selected language applies to installer guidance, the Discord interface, and the agent's base instructions. Set `DISCODEX_LANGUAGE` before running the installer to skip the language prompt. Accepted values are `en`, `ko`, `zh`, and `ja`.

Linux and macOS:

```sh
DISCODEX_LANGUAGE=ko ./install
```

Windows PowerShell:

```powershell
$env:DISCODEX_LANGUAGE = 'ko'
./scripts/install.ps1
```

## Migrating an existing profile

To migrate a profile from an existing Codex Worker installation, run migration with that profile's config file:

```sh
./discodex migrate --config "<path-to-legacy-config.json>"
```

Migration verifies the selected profile's ownership record and service, then copies its configuration, credentials, installation list, and state into the new `discodex` paths. It leaves the original files in place for rollback. Keep the original until the migrated profile has been checked.

The bootstrap scripts also download the platform-specific release binary and verify its digest. They use the `discodex` binary name and default user data path, and do not overwrite an existing target binary.

## First installation

`discodex install` interactively creates the installation list when one does not exist. `install-plan` writes that list without installing the service. Setup asks for the profile display name, Codex executable choice (a detected desktop app or existing CLI, or the official Codex core downloaded by the installer), Discord server and home text channel, transcript and allowed voice channels, operator ID, input policy and registered user/channel IDs, an existing project directory, and an optional model.

The message receive policy can be `Everyone` (all servers and DMs), `RegisteredServer` (the configured server), `RegisteredChannels` (registered channels and their threads), or `Paired` (registered user IDs). The policy authorizes senders; separate allow and block lists are retained, with the block list taking precedence. Sender authorization does not bypass routing rules: home-channel messages, bot mentions, registered channels and their threads, and admitted DMs follow their configured input paths. Slash commands and administration still use the configured operator. A new installation plan applies its selected policy and lists to runtime `message-access.json` once. Reinstallation and updates preserve the current policy and lists.

Provide the Discord token through the installer's secure prompt. The installer checks Discord access and the native connection. If Codex login or MFA is needed, complete it through the official sign-in flow. Optional `effort` and `serviceTier` values in the worker `config.json` apply to a newly started persistent thread. `serviceTier: "off"` clears the native tier. If omitted, native defaults apply; resuming an existing thread does not overwrite its current thread settings with new-thread defaults.

Saving an installation plan alone does not finish setup. The installer checks the native account and catalog, checks Discord configuration, and registers the user service. A running service by itself does not verify text, question/approval, or voice behavior.

## Updating

Pass the existing profile config explicitly so the updater does not guess when several profiles exist. It keeps the saved profile language and does not prompt for a new one.

macOS or Linux:

```sh
./update --config "<path-to-existing-config.json>"
```

Windows PowerShell:

```powershell
./scripts/update.ps1 -Config "<path-to-existing-config.json>"
```

The updater downloads and verifies the current platform's prebuilt executable from the official GitHub Release, then calls `discodex update --source <new-binary> --config <existing-config>` to update only that owned profile. It does not change a source checkout or build from source. `./install --update --config ...` and `./scripts/install.ps1 -Update -Config ...` are aliases.

Updates preserve the profile language, configuration, credentials, and persistent Codex conversation state. If executable replacement or service restart fails, the updater restores the previous owned executable, ownership record, and service state. It does not remove legacy directories kept for rollback. Changes to existing settings or conversation state require a separate explicit migration; updating does not perform that migration.

The in-app release checker follows published GitHub Releases and its persisted schedule. See [Release update behavior](design.md#release-updates).

## Running and removing

The user service is registered as a Linux `systemd --user` unit, a macOS LaunchAgent for the login user, or a Windows logon scheduled task for the current user. Reinstallation and removal check ownership records and affect only resources owned by Discodex. External Codex installations, accounts, conversations, projects, and Discord server history are preserved.

Run uninstall from the installed binary:

```sh
discodex uninstall
```

Uninstall stops the owned service and removes the owned config, Discord credential, registered commands, and installer-managed components. It preserves Codex login and thread state/history, projects, and Discord server history.

## Windows administrator rights and startup

Windows setup prepares the profile and Codex login as the current user. It requests UAC approval once when registering the scheduled task. Approval from the same user account with administrator rights is required. If a different administrator account is used, setup stops without changing the task. If UAC is canceled, prepared files remain but task registration is incomplete.

The task starts the worker at login with Highest privileges. The worker and its child Codex commands therefore use administrator rights; Windows' global UAC policy is not changed. The current account must be an administrator. The worker is not converted into a SYSTEM service that runs while the user is logged out.

An existing Limited task is upgraded only when its executable, config path, user SID, login trigger, and ownership record match. On registration failure, the installer attempts to restore the previous task definition and running state. The task has no runtime limit and is configured to restart after failure up to three times, one minute apart. This restart policy does not detect a Codex turn that remains unresponsive.

References: [Task Scheduler security contexts](https://learn.microsoft.com/en-us/windows/win32/taskschd/security-contexts-for-running-tasks), [How UAC works](https://learn.microsoft.com/en-us/windows/security/application-security/application-control/user-account-control/how-it-works).

## macOS Full Disk Access

macOS does not let the installer check or grant Full Disk Access (FDA) automatically. When a new installation or reinstallation changes the executable path, the installer detects the path change, verifies the owned executable, and opens Finder and the FDA settings pane with that exact executable selected. This appears before native profile setup and LaunchAgent startup. Add and enable the displayed executable only when needed. Press Enter to continue if permission is already granted or unnecessary, or `q` to cancel. Continuing does not prove macOS granted access. Without an interactive terminal, installation stops before starting the service and reports the executable path.

A normal update at the same path with the same signing identity does not reopen the FDA guide. Files & Folders and AppleEvents prompts are separate macOS permissions and are not granted by this guide.

When converting an existing ad-hoc installation with `--migrate-adhoc`, the installer replaces the binary with the signed executable, then opens the same FDA guide before restarting the LaunchAgent. Press Enter to continue or `q` to roll back the previous binary, ownership record, and service state.

To open the guide later:

```sh
discodex mac-permissions --config /path/to/profile/config.json
```

The command selects the actual executable recorded in the ownership data and opens FDA settings. The user must add the executable and enable access. The command does not change or verify the permission itself.

## macOS updates and signing identity

On first macOS installation, Discodex creates a local code-signing key in the login keychain. The installed worker and future updates use the same certificate and fixed identity. The private key is stored as non-exportable, and temporary key files are removed. Key creation and registration are part of the installer. For remote installs, a one-time LaunchAgent performs keychain authentication in the logged-in user's session and is removed afterward. Approve the macOS keychain prompt if shown. If the keychain is locked or key use is denied, installation stops before replacing files.

Signing metadata and the public certificate are kept in `~/Library/Application Support/Discodex/signing/`. If the legacy `Codex Worker/signing/` directory exists and the new directory does not, the whole directory is moved atomically to preserve identity. Preserving the signing identity does not automatically preserve path-based TCC permissions when the executable path changes; use the FDA guide above, and respond to any separate Files & Folders or AppleEvents prompt. The shared signing key and metadata are kept when removing a profile so a reinstall can retain the same identity. If the signing key is deleted, the existing identity cannot be preserved and a replacement is not created automatically. This local signature is not Developer ID signing or notarization for public distribution.

The standard macOS updater downloads and verifies the release executable before applying it. Add `--migrate-adhoc` only for the first conversion of an existing ad-hoc installation. Updates preserve signing identity and restart only the selected profile's service; a service that was stopped stays stopped. Failure restores the previous executable, ownership data, and service state.

The user must check that Full Disk Access remains enabled in macOS settings. Discodex does not edit the TCC database or automate permission grants. References: [Stable code signing and file access](https://developer.apple.com/forums/thread/678819), [Code signing identity and self-signing](https://developer.apple.com/library/archive/technotes/tn2206/_index.html).

## Profile identity and memory

The profile's `name` is its display name and conversation identity. The worker uses it in the base instructions for a new thread and when resuming an existing thread after a service restart. Reconnecting an already-loaded thread does not overwrite its instructions. Conversation history, permissions, tools, and the device's Codex login remain in place.

Realtime voice uses a backend prompt separate from `realtimeStartInstructions`. The worker passes `experimental_realtime_ws_backend_prompt` only to its owned Codex child process; it does not edit global Codex settings. No automatic greeting is injected on connection. Voice receives call boundaries and conversation continuity, then waits for the user's first utterance. Existing conversations do not need to be deleted or recreated to change the profile's base identity.

The optional `memoryEnabled` property in `config.json` controls native memory for this worker. Set it to `true` to enable official memory features, generation and use, dedicated memory tools, and memory generation for the existing thread. Set it to `false` to disable them for this worker. If omitted, the worker follows existing native settings. Reinstallation preserves the property. It does not change global Codex settings or other Codex apps. Native policy controls when memories are extracted and integrated; enabling memory does not promise that every utterance is remembered immediately.

Official references: [Codex config reference](https://learn.chatgpt.com/docs/config-file/config-reference), [thread resume implementation](https://github.com/openai/codex/blob/main/codex-rs/app-server/src/request_processors/thread_processor.rs), [base-instruction priority](https://github.com/openai/codex/blob/main/codex-rs/core/src/session/mod.rs).

## Release updates in Discord

Discodex checks public GitHub Releases as soon as possible on the first run, when no last-check timestamp is stored. Afterward, the persisted last-check timestamp and interval determine when a check is due: six hours after success or 15 minutes after a failed check. Startup honors that due calculation rather than forcing an additional check. A 15-second loop and wake notifications schedule due checks; checks run in the background without creating a Codex turn or user message.

A notice appears in the home channel only while the native connection is available and after the worker has remained idle for five minutes. Idle requires no active turn, voice connection or teardown, pending approval, unprocessed input, message handling, or output delivery. If a check discovers an update after the worker has already met that five-minute idle condition, the notice may appear immediately; discovery does not start another five-minute delay. Notices do not mention the user or trigger push/desktop alerts.

The buttons are **Update**, **Skip this version**, and **Never notify again**. Update pins the selected release, prepares it while work and voice are idle, and checks idle state again before replacing the executable. Skip this version applies only to that release. Never notify again disables notices for that profile; turn notices back on in `/discodex settings`. Buttons and guidance use the installation language.

Updates download the prebuilt asset for the current OS and architecture and verify GitHub's SHA-256 digest and asset size. No client source checkout or Cargo build is used. Discodex preserves the profile config and thread, uses the existing owned-executable update path, and briefly reconnects Discord during application. If the asset is missing or digest verification fails, the update stops and reports the result. Other devices do not update automatically.

Release automation builds six targets (Windows, macOS, and Linux on x64 and ARM64), with binaries, checksums, and `discodex-installers.zip` attached to a draft release. Download the current published assets from the GitHub [latest release](https://github.com/CHOAM-CO/Discodex/releases/latest).

## Work tools after updating

After the worker restarts, the existing Codex thread can use native `update_plan` and the `discodex_tasks` MCP thread and managed-worktree tools. Each changed plan appears as a new home-channel Embed card; earlier cards are never edited. Identical event replays are suppressed, and full plan text is split across pages only when needed to fit Discord limits. No separate MCP installation or user-global Codex configuration edit is needed. New tasks inherit native settings from their caller and do not reset the existing conversation or profile settings.

## Native features, plugins, and operational logs

Native app-server feature choices are owned per profile in `native-features.json`. The feature set includes CodeMode, host, multiagent v2 and `wait_agent`, normal-mode questions, clock, token budget/history notes, and asynchronous notifications. The settings view reports the effective native state separately from the saved values and identifies changes that need a worker restart.

Use `/discodex plugins` to browse the native plugin catalog and invoke supported read, install, and uninstall actions with Discord buttons. Recommendations use native elicitation. Plugin authentication, approval, account availability, and entitlements remain under the official native flow; availability is account- and catalog-dependent.

Asynchronous native questions can be answered from Discord with buttons or free-form text. The original question ID and native thread association are preserved. Asynchronous notifications go to the home channel without ending the active turn.

The main-control event queue is consumed by a supervised worker, and persistent logs are stored per profile. This supports diagnosis of GUI-launched Windows workers without an attached console. Updates install prebuilt release assets; users do not need to build source code on client machines.
