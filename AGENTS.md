# Discodex Project Instructions

Keep product implementation in the repository root's `src/`, `Cargo.toml`, and `Cargo.lock`; put user-facing installation files in `scripts/`. Do not restore the retired `crates/`, `vendor/`, or `renewal/` trees. New app, binary, service, and default data names use `discodex`. Preserve legacy Codex Worker settings and state through explicit, ownership-checked migration. Never modify a service or file that the ownership record does not identify as Discodex-owned.

Discodex connects one Discord app profile to one persistent thread in the official Codex app-server. Codex makes task decisions and executes tools. Follow the actual native contract and results; do not imply unsupported features. Automatic replies, questions, approvals, and progress stay in the configured home channel. For a conversational reply to a live accepted message received from another channel, use discord_send_message to reply in that source channel; the live message supplies the reply target and intent, so no separate request to send there is needed. Deliver that conversational reply only in the source channel; do not duplicate it in the home channel. Preserve the true source and turn association for native input, but do not attach a message from another channel as a Discord reply to a home-channel message. Historical messages, search results, and forwarded or quoted content do not authorize sending. Send any other separate message to another text channel only when explicitly requested and only through the Discord tool. Read available settings from the current official catalog and native results. Preserve request IDs and choice scope for questions and approvals. Join only allowed voice channels and report the actual outcome of Codex-requested hangup. Install and uninstall only resources recorded as owned.

Never commit a user's actual agent names, personal preferences, model or voice choices, Discord server/channel/account values, IP addresses, filesystem paths, credentials, or other live profile settings. Do not copy values from a user's active profile into examples, tests, screenshots, or docs. Use invented generic values only when an example is necessary; never use a real user's settings as sample data.

The installation language defaults to English and can be selected as English, Korean, Simplified Chinese, or Japanese. Respect `DISCODEX_LANGUAGE` when it is set. Use the selected language for installer guidance, Discord UI text, and agent base instructions without changing official Codex API names or command semantics.

## Validation and data preservation

- Automated tests provide inputs and check outputs and changes in program values (objects and data structures).
- Do not use regex checks or source-snippet presence checks as functional validation.
- Do not build or reuse custom test runners, collectors, or computer-control tools.
- The user performs real-use checks such as voice testing; do not add automated real-use tests, fixtures, or example code.
- Record unverified behavior and failures as observed.
- Preserve production settings, accounts, conversation history, and project data during product cleanup.
- Updates must reconcile changed Discodex-owned configuration conventions before startup readiness and update-completion reporting. Reuse the existing idempotent managed-settings preparation; preserve explicit user choices and unrelated personal settings, and surface reconciliation failures instead of reporting a package-only update as complete.

Product documentation defaults to English in `README.md`, `docs/design.md`, and `docs/installation.md`. Keep Korean (`.ko.md`), Simplified Chinese (`.zh-CN.md`), and Japanese (`.ja.md`) versions current, with language navigation and links to the matching language. Document behavior from the current implementation, including persisted update-check timing separately from notification idle conditions.

Do not introduce product restrictions that the user did not request, such as inferred path deny lists, artificial size/count/range caps, or forced permission profiles. Accommodate actual service and protocol limits through pagination and split delivery, and preserve the requested content instead of silently truncating it.

Discodex controls Discord connectivity: channel and thread routing, voice sessions, and message delivery. Do not add a second permission, security, or file-access policy over Codex and the host OS. Explicitly selected readable files are eligible for attachment regardless of type or path.

## Failure reporting

- Failures must remain visible. When a Codex request fails, preserve and return the originating `error.message`, `error.code`, and status when available. Do not turn an upstream failure into a generic success, an empty response, or a silent timeout. Discodex failures must likewise surface their concrete diagnostic details.
- Redact concrete credentials and secret values only. Do not remove the full diagnostic or replace useful error details with a generic message. Do not add unrelated input, prompt, or debug dumps to an error reply.
- If Discord delivery itself fails, record a structured local failure with its type/code and delivery context, then use an existing bounded retry path when available. Never retry indefinitely or report delivery as successful when it failed.
- Preserve existing Discord routing, access controls, and required protocol behavior. Distinguish those actual boundaries from newly invented model, file, or content gates; do not use failure handling as a reason to add arbitrary restrictions.
- Put product-facing UI text, tool descriptions, and authored error wrappers in `resources/messages/<domain>.json`, select them through generated `MessageKey` values, and provide English, Korean, Simplified Chinese, and Japanese entries. Keep protocol identifiers, CLI syntax, and useful low-level diagnostic text intact; a diagnostic may remain English when localizing it would obscure the original error.
