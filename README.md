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

Discodex connects one Discord app profile to one persistent conversation. Text and voice share Codex context; an optional Antigravity CLI backend supports text conversations. The bundled Codex fork manages native tasks and subagents.

This page describes the 1.0 source tree. Downloadable binaries follow the published release tags.

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

Antigravity preferences apply on the next model selection or AGY session start; native process feature switches require a worker restart. Enable **Antigravity** in Settings to discover exact model IDs and labels through the installed CLI’s `agy models` command for both pickers; manually entered IDs supplement that list. Effort choices come from CLI `--help`, without model-name guesses or remapping. Catalog failures are shown in settings, and an explicitly requested unsupported effort is passed through so the CLI can report its error. Discodex uses a separately installed AGY CLI and its existing subscription login, not the SDK. AGY executes its overlapping built-in tools; additional Discord and task tools connect through MCP. Subagent lifecycle remains with the Codex fork. Each response starts a fresh CLI execution with native conversation and visible tool-result context; private AGY state is not resumed. AGY voice is not supported.

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

The settings panel offers the main model, default subagent model, reasoning effort, service tier, permission profile, approval policy, input delivery, voice profile, and release notices. Approval defaults to **on-request**, independently of the permission profile. **Advanced settings** contains detailed native feature switches; it does not repeat the subagent-model picker. Voice changes apply to the next call. Detail screens return to the settings menu with Back; Exit closes the panel. Approval choices are saved per profile in `native-features.json`; they do not overwrite shared Codex configuration.

Leaving voice disconnects the call while keeping the persistent conversation. Use `/discodex new` to start a different conversation.

## Documentation

- [Installation and removal](docs/installation.md)
- [Product design](docs/design.md)
