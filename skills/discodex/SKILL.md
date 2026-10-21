---
name: discodex
description: Explain, configure, update, and troubleshoot Discodex, the Discord bridge running this agent. Use for Discodex settings, supported behavior, message or file delivery, voice connections, and installation questions.
---

# Discodex

This guide is bundled with Discodex **{{version}}**. Use the references for this installed version and the current tool results when answering questions about the bridge.

Read the relevant sections only:

- [Installation and configuration](references/installation.md): profile settings, languages, providers and models, approval settings, updating, platform services, and removal.
- [Behavior and architecture](references/design.md): home-channel and source-channel replies, attachments and tables, voice inbox and transcripts, persistent context, task tools, memory, and update lifecycle.

Discodex connects Discord to the native Codex runtime. Distinguish Discord delivery, worker behavior, native model or tool execution, provider responses, and host OS behavior when diagnosing a problem. Use the actual error and current settings to identify the responsible component; a running worker alone does not prove delivery or voice is working.

The available tool schemas and returned catalogs describe what this session can do. Use them when making a change; the documentation explains the workflow but does not establish that an action succeeded. Describe the result that was actually observed.

For a newer release, consult the [official releases](https://github.com/CHOAM-CO/Discodex/releases). The bundled references come from this version's [design](https://github.com/CHOAM-CO/Discodex/blob/v{{version}}/docs/design.md) and [installation](https://github.com/CHOAM-CO/Discodex/blob/v{{version}}/docs/installation.md) documents. Relative links to other languages or repository pages can be followed from those source pages.
