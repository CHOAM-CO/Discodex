# Validation status

Recorded on 2026-10-15 UTC for the current source.

## Automated checks

| Platform | Unit tests passed | Failed / skipped | Release build |
| --- | ---: | --- | --- |
| macOS | 224 | 0 / 0 | Passed |
| Linux | 214 | 0 / 0 | Passed |
| Windows | 215 | 0 / 0 | Passed |

The library suite runs with `cargo test --locked --offline --lib`. It checks implementation inputs, outputs, and changes in program values. Existing compiler warnings remain. No separate external validation tool or automated Discord/voice usage test is included.

The Unix install and update scripts passed shell syntax checks. The Windows install and update scripts passed the official Windows PowerShell 5.1 parser. The install script includes a UTF-8 BOM so PowerShell 5.1 reads its translated text correctly.

## User checks

- During a call, queue home-channel messages and mentions from other channels; accept text from the current transcript thread; drain queued messages after the call.
- Keep consecutive calls in separate transcript threads. Check first-response latency, audio continuity, voice selection, agent identity, hangup, and summaries.
- Check Discord attachments, explicit message delivery, question and approval UI, settings, and memory behavior.
- Check OS permission dialogs, installation/removal/update behavior, and startup after login or restart.

See [user validation](user-validation.md) for the full checklist. A running service and passing unit tests do not establish real-use behavior, model intent handling, or tool selection.

A call-start message can still appear in the transcript thread before a voice connection fails. That message alone does not confirm a successful connection.
