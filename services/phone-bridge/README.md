# PhoneBridge

PhoneBridge connects SIP calls to Codex Voice through native WebRTC. Codex CLI runs
in the background without a terminal or Desktop app. Voice needs no Micro emulator
or virtual audio cable. Windows calls and announcement screening use the separately
configured `audio` settings. See [Voice requirements](../../specs/PHONE-VOICE.md) and
the [service schema](../../specs/schemas/phone-service.schema.json).

Configure `codexVoice` with the pinned executable/version/hash, Codex home, working
directory and optional native project ID. Its home and app-server options follow
AgentManager's shared runtime rules and remain independently configurable. Native
instructions, MCP tools and approval policy come from the selected home. A shared
daemon uses an invisible proxy; `owned-stdio` selects a service-owned background
app-server. Closing PhoneBridge leaves a shared daemon running.

The incoming principal's task is prepared while idle, and each principal reuses its
recorded task. `keepTaskLoaded` defaults to true; false unsubscribes the prepared
task while retaining its identity for later resume. First creation and tool loading
can take longer than a warm call. SIP settings, credentials, access challenges and
screening remain protected installation configuration.

`phone_bridge_status` reports configured recipients, calls, model selection and
original operations. `phone_bridge_call` requires `recipientId`, `operationId` and
the complete `initialPrompt`. An active Voice call for that recipient receives the
new prompt in its existing task. Replaying an operation never sends it twice.
`incomingInitialPrompt` controls incoming greetings. Each session receives its full
prompt at realtime startup, including when the task is reused.

The default task model is Sol/high. `phone_bridge_select_voice` updates following
turns; `phone_bridge_restart_voice` prepares a new task and reconnects Voice within
the same phone call. The keypad equivalents are `*1<M><R>#` for model/reasoning and
`*0#` for a new task. Accepted commands receive audible feedback. The prior task
remains available as conversation history.

`phone_bridge_hangup` revokes call audio immediately, then confirms CLI session stop,
peer disposal and admission release. A failed transport or media route ends the
original call and retains its outcome. Use `phone.history`, `phone.operation` and
`phone.logs` to inspect it. SIP and Windows audio remain independent of idle CLI
readiness checks.
