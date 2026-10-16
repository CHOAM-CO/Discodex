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

Discodex connects one Discord app profile to one persistent conversation with the official Codex app-server. Chat and voice share that conversation. Codex handles task decisions and tool execution.

## Features

| Feature | What it does |
| --- | --- |
| Persistent context | Chat and voice use the same Codex conversation, which resumes after a worker restart or voice call. |
| Voice and transcripts | Talk in an allowed voice channel. Confirmed transcripts are kept in a per-call Discord thread. |
| Questions and approvals | Receive Codex questions and approval requests in Discord forms and buttons; return selections to the matching request. |
| Agent settings | Set a profile name and memory behavior. Choose supported models, reasoning effort, and permissions in the `/discodex settings` panel. |
| Discord context and attachments | Read messages, channels, and users; pass images and files into the conversation; send a separate message when explicitly requested. |

## Getting started

Install from source. You need **Rust 1.94 or later**, Cargo, and the build tools for your operating system.

```sh
git clone https://github.com/CHOAM-CO/Discodex.git
cd Discodex
```

On macOS or Linux, run:

```sh
./install
```

On Windows PowerShell, run:

```powershell
./scripts/install.ps1
```

The installer lets you choose English (default), Korean, Simplified Chinese, or Japanese. That language is used for installation prompts, Discodex's Discord interface, and the agent's base instructions. You can preselect it with `DISCODEX_LANGUAGE=en`, `ko`, `zh`, or `ja`; a valid existing value skips the language prompt.

The setup connects your Codex installation and Discord bot, then configures a profile and a user-level startup service. See the [installation guide](docs/installation.md) for migration, update, removal, and macOS permissions.

## Updating

From a clean source checkout, update the selected profile with its existing config file:

```sh
./update --config "<path-to-existing-config.json>"
```

On Windows PowerShell:

```powershell
./scripts/update.ps1 -Config "<path-to-existing-config.json>"
```

The updater fast-forwards the checkout only when tracked files have no local changes, builds the release binary, and applies it to the profile named by `--config`. It does not prompt for a language or rewrite profile settings. The selected language, credentials, and persistent Codex conversation state stay with that profile. If an owned binary replacement fails, the updater restores the previous binary and service state. `./install --update --config ...` and `./scripts/install.ps1 -Update -Config ...` are aliases.

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

Settings panels show the current selection and buttons for models, reasoning effort, Fast mode, permission profiles, input delivery, and voice profiles. Voice changes apply to the next call.

Leaving a voice call disconnects voice only; the persistent conversation remains available. Use `/discodex new` to start a new conversation.

## Documentation

- [Installation and removal](docs/installation.md)
- [Product design](docs/design.md)
