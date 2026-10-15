# Discodex Project Instructions

Keep product implementation in the repository root's `src/`, `Cargo.toml`, and `Cargo.lock`; put user-facing installation files in `scripts/`. Do not restore the retired `crates/`, `vendor/`, or `renewal/` trees. New app, binary, service, and default data names use `discodex`. Preserve legacy Codex Worker settings and state through explicit, ownership-checked migration. Never modify a service or file that the ownership record does not identify as Discodex-owned.

Discodex connects one Discord app profile to one persistent thread in the official Codex app-server. Codex makes task decisions and executes tools. Follow the actual native contract and results; do not imply unsupported features. Ordinary replies, questions, approvals, and progress stay in the configured home channel. Preserve the true source and turn association for native input, but do not attach a message from another channel as a Discord reply to a home-channel message. Send a separate message to another text channel only when explicitly requested and only through the Discord tool. Read available settings from the current official catalog and native results. Preserve request IDs and choice scope for questions and approvals. Join only allowed voice channels and report the actual outcome of Codex-requested hangup. Install and uninstall only resources recorded as owned.

Never commit a user's actual agent names, personal preferences, model or voice choices, Discord server/channel/account values, IP addresses, filesystem paths, credentials, or other live profile settings. Do not copy values from a user's active profile into examples, tests, screenshots, or docs. Use invented generic values only when an example is necessary; never use a real user's settings as sample data.

The installation language defaults to English and can be selected as English, Korean, Simplified Chinese, or Japanese. Respect `DISCODEX_LANGUAGE` when it is set. Use the selected language for installer guidance, Discord UI text, and agent base instructions without changing official Codex API names or command semantics.

## Validation and data preservation

- Automated tests provide inputs and check outputs and changes in program values (objects and data structures).
- Do not use regex checks or source-snippet presence checks as functional validation.
- Do not build or reuse custom test runners, collectors, or computer-control tools.
- The user performs real-use checks such as voice testing; do not add automated real-use tests, fixtures, or example code.
- Record unverified behavior and failures as observed.
- Preserve production settings, accounts, conversation history, and project data during product cleanup.

Product documentation lives in `docs/design.md` and `docs/installation.md`.
