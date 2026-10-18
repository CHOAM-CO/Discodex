<p align="center">
  <img src="docs/assets/discodex-banner.png" alt="Discodex — conversations by text and voice" width="960" />
</p>

<p align="center">
  <strong>Talk in Discord and keep the same context.</strong><br />
  A Rust bridge connecting text and voice to an ongoing conversation with the official Codex app-server.
</p>

<p align="center">English · <a href="README.ko.md">한국어</a> · <a href="README.zh-CN.md">简体中文</a> · <a href="README.ja.md">日本語</a></p>
<p align="center"><a href="#getting-started">Getting started</a> · <a href="docs/installation.md">Installation</a> · <a href="docs/design.md">Design</a></p>
<p align="center"><sub>macOS · Linux · Windows</sub></p>

---

Discodex connects one Discord app profile to one persistent conversation with the official Codex app-server. Text and voice share the conversation. Codex handles task decisions and tool execution.

## Features

| Feature | What it does |
| --- | --- |
| Persistent context | Chat and voice share a Codex conversation that resumes after a worker restart or voice call. |
| Voice and transcripts | Talk in an allowed voice channel. Confirmed transcripts are kept in a per-call Discord thread. |
| Questions and approvals | Answer Codex questions and approval requests in Discord forms and buttons; selections return to the matching request. |
| Profile and memory | Set a profile name and, with `memoryEnabled: true`, use official native memory features and dedicated memory tools. |
| Message routing and mentions | Choose which senders are authorized with Everyone, RegisteredServer, RegisteredChannels, or Paired, plus independent allow and block lists. Routing also applies the home-channel, mention, registered-channel/thread, and admitted-DM rules. Normal replies can mention the message author, including for home-channel messages. |
| Voice inbox | Get a short preview of messages received during a call; unread messages are handled after the call, with attachments checked before an item is marked read. |
| Discord context and files | Read messages, channels, and users; pass images and files into the conversation; send a separate message when explicitly requested. |
| Markdown tables | Completed ordinary assistant replies and `discord_send_message` output render tables as wrapped, paginated PNGs while keeping surrounding prose; a button lets the clicking user view the paginated Markdown source. |
| Plans and task tools | Each changed Codex plan gets a new home-channel Embed card, never an edit to the prior card. Cards show completed/total counts, completion check and strike-through, active-step arrow, pending marker, and status color; identical replays are suppressed, and full plan text is paged at Discord’s actual limits without truncation. Manage threads and worktrees through `discodex_tasks`. |
| Release updates | Receive an idle-time notice for published releases and update from a verified prebuilt executable. |
| Native features | Profile-owned `native-features.json` enables supported official app-server capabilities; settings distinguish effective state from saved values and identify restart-required changes. |
| Plugins | Browse the current native plugin catalog and use Discord buttons for supported read, install, and uninstall flows in `/discodex plugins`; native recommendations and authorization remain in the official flow. |
| Async questions and notices | Answer native questions with Discord buttons or free-form responses tied to their original question and thread. Async notifications appear in the home channel without ending the active turn. |
| Reliability and logs | A supervised consumer handles main-control events, with persistent profile-scoped logs for GUI-launched Windows workers. Updates use prebuilt releases, so client machines do not build from source. |

## Getting started

Download `discodex-installers.zip` from the [latest release](https://github.com/CHOAM-CO/Discodex/releases/latest), extract it, and open the extracted folder in a terminal. Installers download the prebuilt executable for your operating system and architecture. Client installation does not require Rust, Cargo, Git, or build tools.

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

## Updating

Use the updater from the installer bundle with the existing profile config:

```sh
./update --config "<path-to-existing-config.json>"
```

On Windows PowerShell:

```powershell
./scripts/update.ps1 -Config "<path-to-existing-config.json>"
```

The updater downloads the matching prebuilt release executable, verifies its SHA-256 digest, and applies it to the profile named by `--config`. The Discord Update button uses the same release assets. Client machines do not need Git, Cargo, or a compiler. `./install --update --config ...` and `./scripts/install.ps1 -Update -Config ...` are aliases.

Release checks use the persisted last-check timestamp: every six hours after success and every 15 minutes after failure. The first check is due immediately; due checks run through a 15-second loop and wake notifications. A notice appears only while the native connection is available and after five continuous minutes with no active turn, voice call or teardown, pending approval or input, message handling, or output delivery. Discovery adds no wait when the worker has already met that idle condition. **Update** applies the selected release, **Skip this version** suppresses that release, and **Never notify again** disables notices for that profile. Re-enable notices in `/discodex settings`.

Profiles on versions older than v0.1.3 need a one-time manual update from the installer bundle to install the self-updater. This update preserves the existing session and settings. Afterward, Discord release updates are available from the notice buttons.

## Discord commands

| Action | Command |
| --- | --- |
| Choose settings with buttons | `/discodex settings` |
| Choose a voice profile | `/discodex voice` |
| Show the current conversation and settings | `/discodex status` |
| Join an allowed voice channel | `/discodex voice-join channel:<voice channel>` |
| Leave the voice call | `/discodex voice-leave` |
| Request that the current response stop | `/discodex interrupt` |
| Start a new conversation | `/discodex new` |

The settings panel offers supported models, reasoning effort, service tier, permission profile, input delivery, voice profile, and release notices. Voice changes apply to the next call. Detail screens return to the settings menu with Back; Exit closes the panel.

Leaving voice disconnects the call while keeping the persistent conversation. Use `/discodex new` to start a different conversation.

## Documentation

- [Installation and removal](docs/installation.md)
- [Product design](docs/design.md)
