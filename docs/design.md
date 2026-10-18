# Product design

Discodex connects one Discord app profile to one persistent thread in the official Codex app-server. Discodex routes Discord messages, requests, approvals, settings, and voice media; Codex makes task decisions and runs tools. New installs use `discodex` for the binary, service, and default data paths. Existing Codex Worker profiles can be migrated explicitly from a config file, with the original preserved for rollback.

<p align="center">English · <a href="design.ko.md">한국어</a> · <a href="design.zh-CN.md">简体中文</a> · <a href="design.ja.md">日本語</a></p>

## Conversation and settings

The configured home text channel is the persistent conversation surface for ordinary replies, questions, approvals, status, and plans. A worker restart resumes the saved thread. `/discodex new` starts another thread only when requested. Restarting does not promise to preserve an in-progress response or voice call.

`/discodex settings` provides buttons for supported models, reasoning effort, service tier, permission profile, input delivery, voice profile, message receive policy, allow/block lists, and update notifications. Options come from the current native catalog and are checked again when selected. Lists are paginated to fit Discord's limits without dropping options. Detail screens have a Back button to return to the settings menu; Exit closes the settings view. Neither action rolls back settings already applied. Voice choices take effect on the next call. The settings panel and prompts follow the selected installation language.

The message-policy controls can select a receive policy and manage allowed or blocked users and channels. User choices show names or nicknames resolved from the current server, while adding or removing a person uses their account ID. Voice choices come from `thread/realtime/listVoices`. Choosing the Codex default omits an explicit voice so native settings apply. A selected voice is stored in worker state and reused on the next connection.

Slash commands include `/discodex settings`, `/discodex voice`, `/discodex status`, `/discodex interrupt`, `/discodex new`, `/discodex voice-join`, and `/discodex voice-leave`. Slash commands and administrative actions retain their configured operator checks. During an active ordinary Discord-message turn, the home channel typing indicator is refreshed and stopped on completion, error, or disconnect.

When native Codex asks a question or requests approval, Discodex displays a Discord form or buttons and returns the response with the original request ID and choice scope. Old controls cannot answer a resolved request or a request from a different connection generation.

## MCP tool-call approvals

An official Codex form elicitation marked `_meta.codex_approval_kind: "mcp_tool_call"` with an empty object schema is shown as execution approval buttons rather than a free-form input form. Discodex offers the approval actions supported by the native request and exposes session-wide or always-allow choices only when the request's `_meta.persist` provides that scope. It passes the selected `action` and native `_meta.persist` through without inventing input or credentials. Ordinary forms remain ordinary input; URL or user-authentication requests are not treated as tool-call approvals. Request IDs, connection generations, and duplicate-response protection remain attached to each pending request. See the [official approval form handling](https://github.com/openai/codex/blob/740e5af33c71225640e0c1c1555c514c2c93ab74/codex-rs/core/src/mcp_tool_call.rs#L1884) and [app-server response metadata forwarding](https://github.com/openai/codex/blob/740e5af33c71225640e0c1c1555c514c2c93ab74/codex-rs/app-server/src/bespoke_event_handling.rs#L1689).

## Discord message routing

The configured receive policy determines which senders are authorized: `Everyone`, `RegisteredServer`, `RegisteredChannels`, or `Paired`. Separate allow and block lists remain in effect across policy changes; the block list takes precedence over the allow list. The bot ignores its own messages. Authorization alone does not determine the input path: routing still applies the home-channel, bot-mention, registered-channel/thread, and admitted-DM rules. A mention does not grant sender authorization.

Accepted messages use the existing input route. Home-channel messages can be linked as Discord replies. Messages from other channels retain their source and receive a source link; they are not attached as replies to a home-channel message. Ordinary answers, questions, approvals, and progress stay in the home channel. Sending a separate message to another text channel requires an explicit user request and uses the Discord send tool.

The native input includes source author, body, and a stable `source.authorMention` token, including for home-channel messages and bot-authored messages. Explicit user mentions are enabled on normal conversational replies so the agent can address the input author. Status, error, and other non-conversational notices suppress mentions. Bot-authored messages include `source.authorIsBot: true` so Codex can distinguish them from human input.

During a voice call, messages in that call's transcript thread can be routed directly into the voice conversation. Other accepted messages that arrive during voice setup, an active call, or teardown are saved in a durable inbox. The call receives a short notification preview; the preview does not mark the item read or request that it be spoken. Inbox messages and attachment details are persisted before download; failed attachments are retried and any continuing failure is reported. An item is marked read only after every attachment is received and the associated native turn completes. The inbox can be read during the call through `discord_read_voice_inbox` when the content is relevant or the user asks. After confirmed teardown, only items still unread are submitted in order once existing native work is idle.

Discord tools can list channels and users, read and search messages, download selected attachments, and send messages. Channel and user names are resolved against current server data; ambiguous results return choices. Message and attachment operations can target a thread by ID, including archived threads, after checking current server access. Message pagination follows Discord cursors and API page sizes; it does not invent a fixed history index. Tool output preserves source IDs and native turn association.

Explicitly selected readable local files can be attached regardless of type or path. Direct message attachments are downloaded before input is submitted. PNG, JPEG, and WebP files are passed as native image input; other files are provided by local path for Codex to read. The app-server does not expose a general binary file turn item, so this is not described as equivalent to a ChatGPT file-upload API. Files linked in completed assistant answers can be delivered as Discord attachments. Discord's actual request limits are handled by splitting delivery; Discodex does not add its own file-type, path, or aggregate-size deny list.

## Voice

`/discodex voice-join` and the agent `voice_join` tool report success only after the actual Codex Realtime WebRTC connection is ready in an allowed voice channel. They carry the context of the same persistent thread. Configured operator audio is sent to Realtime, response audio is played in Discord, and confirmed transcripts are written to a per-call transcript thread. Each call uses its own Realtime session ID while retaining the backing thread ID, so the transcript belongs to that call and conversation context continues.

The `name` profile setting supplies the text conversation identity on new threads and when resuming a thread after service restart. Realtime voice uses a separate backend identity prompt: the worker passes `experimental_realtime_ws_backend_prompt` to its owned Codex child process. This backend override is distinct from `realtimeStartInstructions` and does not change global Codex settings. No automatic greeting is injected; voice receives the call boundary and thread continuity, then waits for the user's first utterance.

`/discodex voice-leave` ends the call and reports the actual outcome. When Codex requests hangup, the `voice_leave` tool uses its generated call summary to update the call's captured parent anchor after successful transcript cleanup. Manual `/discodex voice-leave` does not generate or attach a summary. Leaving voice preserves the backing Codex thread and its text conversation.

Normal 20 ms Opus audio is forwarded compressed when possible. Songbird handles Discord-specific RTP headers and encryption. Realtime calls use a unique `realtimeSessionId` for each call while retaining the existing `threadId`, so each transcript belongs to the correct call and conversation context continues. No automatic greeting is injected when the call connects. Unconfirmed transcript text is not published as a confirmed utterance.

Messages with attachments in the active transcript thread enter the same persistent Codex conversation with text and files together. If any required attachment download fails, the message is not partially submitted. Other home-channel messages and bot mentions received during a call remain in the inbox until teardown.

## Work tools and plans

The `discodex_tasks` MCP exposes the official Codex CLI thread tools (`list_threads`, `list_archived_threads`, `read_thread`, `wait_threads`, `send_message_to_thread`, `create_thread`, `fork_thread`, `set_thread_title`, and `set_thread_archived`) and managed-worktree tools (`create_worktree`, `get_worktree_creation_status`, and `list_worktrees`). The tools use the worker-owned app-server process and do not edit the user's global Codex configuration. Other threads remain separate tasks; their replies are not mixed into the profile's home channel or voice transcript.

Native `update_plan` progress is shown in a home-channel message associated with the current thread, connection generation, and turn. Discord length limits are handled by splitting the content. This progress view is separate from switching Codex into Plan mode.

## Memory

When `memoryEnabled` is `true` in the worker profile, the worker enables the official native memory features, generation and use, and dedicated memory tools. It also aligns the resumed thread's memory mode. If the field is omitted, existing native memory settings apply. This setting affects the worker's Codex child process and does not change global Codex settings or other Codex processes. Native policy determines when memories are created and integrated; each utterance is not guaranteed to be remembered immediately.

## Release updates

Discodex checks public GitHub Releases as soon as possible on the first run, when no last-check timestamp is stored. Afterward, the persisted last-check timestamp and interval determine when a check is due: six hours after a successful check or 15 minutes after a failed check. Startup honors that due calculation and does not force an extra check. A 15-second loop and wake notifications schedule due checks; release metadata checks do not create a Codex turn or a user message.

If a newer release is available, Discodex posts a version notice only while the native connection is available and after the worker has remained idle for five minutes. Idle means there is no active turn, voice connection or teardown, pending approval, unprocessed input, message handling, or output delivery. If discovery finds the release after the worker has already met that five-minute idle condition, the notice can be posted immediately; discovery does not start another five-minute wait. The notice does not mention the user or trigger push/desktop notifications.

The notice offers **Update**, **Skip this version**, and **Never notify again**. Update pins the selected release and applies it only after checking idle state again. Skip this version applies only to that release. Never notify again disables notices for this profile; notifications can be re-enabled in settings. The release is installed from the prebuilt asset for the current OS and architecture. The client verifies GitHub's SHA-256 digest and asset size, then replaces the owned executable through the updater. No source checkout, Git, Cargo, or build tools are needed on client machines. The persistent thread, configuration, and supported platform signing identity are preserved; applying an update briefly reconnects Discord. An unavailable asset or digest mismatch stops the update and reports the result.

Release automation builds six prebuilt targets (Windows, macOS, and Linux, each x64 and ARM64) and attaches the binaries, checksums, and installer archive to the draft release. Download the installer archive and platform assets from the GitHub [latest release](https://github.com/CHOAM-CO/Discodex/releases/latest).

## Installation ownership

The installer registers a user-scoped Linux systemd user service, macOS LaunchAgent, or Windows logon scheduled task. It records owned resources and only updates or removes those resources. It preserves Codex credentials, threads, projects, Discord history, and external Codex installations. See the [installation guide](installation.md) for setup, migration, update, removal, and platform permissions.
