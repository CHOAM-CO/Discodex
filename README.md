<p align="center">
  <img src="docs/assets/discodex-banner.png" alt="Discodex — conversations by text and voice" width="960" />
</p>

<p align="center">
  <strong>Talk in Discord and keep the same context.</strong><br />
  A Rust bridge connecting Discord to a persistent conversation, powered by a bundled Codex fork.
</p>

<p align="center">English · <a href="README.ko.md">한국어</a> · <a href="README.zh-CN.md">简体中文</a> · <a href="README.ja.md">日本語</a></p>
<p align="center"><a href="#getting-started">Getting started</a> · <a href="docs/installation.md">Installation</a> · <a href="docs/design.md">Design</a></p>
<p align="center"><sub>macOS · Linux · Windows</sub></p>

---

Discodex connects one Discord app profile to a persistent working conversation. Codex or the optional Antigravity CLI executes work; OpenAI Realtime provides voice and delegates to that same working conversation. The bundled Codex fork manages native tasks and subagents.

This page describes the current source tree. Downloadable binaries follow the published release tags.

## Features

| Feature | What it does |
| --- | --- |
| Persistent context | Chat and voice share a Codex conversation that resumes after a worker restart or voice call. |
| Voice and transcripts | Talk in an allowed voice channel. Confirmed transcripts are kept in a per-call Discord thread. |
| Questions and approvals | Answer Codex questions and approval requests in Discord forms and buttons; selections return to the matching request. |
| Profile and memory | Set a profile name and, with `memoryEnabled: true`, use official native memory features and dedicated memory tools. |
| Bundled Discodex skill | Provides a guide in the installation language with setup, settings, and troubleshooting references for the installed build; updates refresh managed files. |
| Message routing and mentions | Choose which senders are authorized with Everyone, RegisteredServer, RegisteredChannels, or Paired, plus independent allow and block lists. Routing also applies the home-channel, mention, registered-channel/thread, and admitted-DM rules. Normal replies can mention the message author, including for home-channel messages. |
| Voice inbox | Get a short preview of messages received during a call; unread messages are handled after the call, with attachments checked before an item is marked read. |
| Discord context and files | Read messages, channels, and users; pass images and files into the conversation; send a separate message when explicitly requested. |
| Markdown tables | Completed ordinary assistant replies and `discord_send_message` output show tables as wrapped, paginated PNGs, preserving inline bold, italic, and underline while keeping surrounding prose. A localized **View source** button shows copyable Markdown to the clicking user. |
| Plans and task tools | Each changed Codex plan gets a new home-channel Embed card, never an edit to the prior card. Cards show completed/total counts, completion check and strike-through, active-step arrow, pending marker, and status color; identical replays are suppressed, and full plan text is paged at Discord’s actual limits without truncation. Manage threads and worktrees through `discodex_tasks`. |
| Release updates | Receive an idle-time notice for published releases and update from a verified prebuilt executable. |
| Native features | Profile-owned `native-features.json` enables supported official app-server capabilities; settings distinguish effective state from saved values and identify restart-required changes. |
| Plugins | Browse the current native plugin catalog and use Discord buttons for supported read, install, and uninstall flows in `/discodex plugins`; native recommendations and authorization remain in the official flow. |
| Async questions and notices | Answer native questions with Discord buttons or free-form responses tied to their original question and thread. Async notifications appear in the home channel without ending the active turn. |
| Reliability and logs | A supervised consumer handles main-control events, with persistent profile-scoped logs for GUI-launched Windows workers. Updates use prebuilt releases, so client machines do not build from source. |

Paired users and users on the allow list can send DMs without mentioning the bot, regardless of the selected server/channel receive policy. The block list and self-message exclusion still take precedence. `Everyone` also accepts other DM senders. Ordinary automatic output, questions, approvals and progress remain in the home channel; to answer a live DM in the same conversation, the agent uses `discord_send_message` with its `source.dm_id`.

## Getting started

Download `discodex-installers.zip` from the [latest release](https://github.com/CHOAM-CO/Discodex/releases/latest), extract it, and open the extracted folder in a terminal. Installers download the prebuilt worker and matching managed Codex runtime for your operating system and architecture. Choose an existing Codex data home to share account, memory and history (recommended when found), or a separate home for Discodex. Shared Codex settings are preserved. Client installation does not require Rust, Cargo, Git, or build tools.

On macOS or Linux, run:

```sh
./install
```

On Windows PowerShell, run:

```powershell
./scripts/install.ps1
```

The installer offers English (default), Korean, Simplified Chinese, and Japanese. The selected language is used for installation guidance, Discodex's Discord interface, and the agent's base instructions. You can set `DISCODEX_LANGUAGE=en`, `ko`, `zh`, or `ja` before installation to select a language.

Setup connects the Codex app-server and Discord bot, configures a profile, and registers a user-level startup service. See the [installation guide](docs/installation.md) for migration, updates, removal, and macOS permissions.

## Custom and local models

The main-model and **Default subagent model** pickers include the native catalog and explicitly configured provider/model pairs. The bundled fork allows a child to use a different provider from its parent. Saved child defaults are loaded on the next thread creation or resume, including after a worker restart. The current thread keeps its effective settings until resumed; existing children and the parent conversation remain intact. Codex still owns provider authentication and model capabilities. See the [provider guide](docs/installation.md#custom-and-local-codex-providers).

Provider history compatibility uses independent `encrypted_content` and `agent_messages` capabilities. Outbound conversion preserves readable context and leaves saved history untouched; see [history compatibility](docs/design.md#provider-changes-and-encrypted-history).

Open **Settings → Antigravity** to install and sign in to a profile-managed official AGY CLI. Setup offers existing preferences as a read-only baseline or separate preferences, and can be canceled or exited. Browser consent is completed by the user. Discodex always keeps its MCP registration, scoped tool allow rule and settings overrides in private profile files; personal AGY configuration is not modified. The OS account keyring follows AGY’s native behavior. Removal deletes the managed CLI and preserves external AGY installations and personal settings.

The AGY catalog is cached for 24 hours; use **Settings → Antigravity → Update model list** to refresh it. The CLI receives a base model ID and a separate `--effort`. Supported levels come from the discovered model metadata. When a selected level is unavailable, it is lowered to the highest supported level at or below the selection, and settings show the actual value. Unknown model IDs are preserved and CLI errors are surfaced. Codex `ultra` retains its orchestration guidance even when the native AGY effort maps to a lower level.

Each AGY execution starts a CLI process, resumes its stored exact conversation ID, and sends only newly accepted input and pending transcript context. Switching between Codex and AGY starts a fresh working session and keeps references for explicit history lookup. Child agents start fresh by default; explicit context forks remain available for supported Codex children, while an unsupported AGY fork returns its concrete error. Codex manages child lifecycle through MCP.

Voice uses the selected Codex account’s OpenAI Realtime connection with any main executor. Delegated results return to the current call; speech after the last handoff is saved at call end without generating another response, and reaches the next normal request. Changing the primary session during a call leaves that call on its original working session. Native and Discodex failures retain their originating message, code and status when available.

## Updating

Run `/discodex update` to check immediately. The **Update / Cancel** confirmation explains that applying an update interrupts active work, including subagents, and ends voice. Confirmation applies the release without an idle queue. Completion or failure is posted in the home channel after restart; interrupted responses are not automatically restarted, and no resume prompt or empty-input continuation is inserted.

Automatic checks run every six hours, retry failures after 15 minutes, and show notices after five minutes of inactivity. Notices offer **Update**, **Skip this version**, and **Never notify again**. These timing rules control notices, not an update you confirm.

For a manual update, use the installer bundle:

```sh
./update --config "<path-to-existing-config.json>"
```

```powershell
./scripts/update.ps1 -Config "<path-to-existing-config.json>"
```

The updater downloads the worker and its matching prebuilt Codex package. Its manifest binds the worker version, fork commit, platform and checksums. It stages the runtime and replaces only the selected profile's owned files. On replacement or restart failure, the updater attempts to restore the previous worker, profile configuration, runtime references, ownership record, and service state. If a required recovery step fails, dependent steps stop and the cause is reported; a mixed installation is not restarted. Unused staged runtimes are cleaned up after any required service recovery, and cleanup failures are also reported. Codex home, account, memory, history and profile settings stay in place. Existing 0.1.x self-updaters retain the same executable asset names and helper entrypoint; installations before 0.1.3 need the manual script first. Client machines do not compile source.

## Discord commands

| Action | Command |
| --- | --- |
| Choose settings with buttons | `/discodex settings` |
| Choose a voice profile | `/discodex voice` |
| Show the current conversation and settings | `/discodex status` |
| Join an allowed voice channel | `/discodex voice-join channel:<voice channel>` |
| Leave the voice call | `/discodex voice-leave` |
| Request that the current response stop | `/discodex interrupt` |
| Compact the current conversation context | `/discodex compact` |
| Check for and apply updates | `/discodex update` |
| Start a new conversation | `/discodex new` |

The settings panel offers the main model, default subagent model, reasoning effort, service tier, permission profile, approval policy, input delivery, voice profile, and release notices. Approval defaults to **on-request**, independently of the permission profile. **Antigravity** has its own settings-home entry. **Advanced settings** contains detailed native feature switches; it does not repeat the subagent-model picker. Voice changes apply to the next call. Detail screens return to the settings menu with Back; Exit closes the panel. Approval choices are saved per profile in `native-features.json`; they do not overwrite shared Codex configuration.

Leaving voice disconnects the call while keeping the persistent conversation. Use `/discodex new` to start a different conversation.

## Documentation

- [Installation and removal](docs/installation.md)
- [Product design](docs/design.md)
