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

An unused task is prepared while idle and claimed by the next call. `keepTaskLoaded`
defaults to true; false unsubscribes the prepared
task while retaining its identity for later resume. First creation and tool loading
can take longer than a warm call. SIP settings, credentials, access challenges and
screening remain protected installation configuration.

`phone_bridge_status` reports configured recipients, calls, model selection and
original operations. `phone_bridge_call` requires `recipientId`, `operationId` and
the complete `initialPrompt`. An active Voice call for that recipient receives the
new prompt in its existing task. Replaying an operation never sends it twice.
`incomingInitialPrompt` controls incoming greetings. Each session receives its full
assignment at startup, including when the task is reused. A short connected cue
opens the conversation once audio connects. Phone-specific instructions distinguish
the assignment from the recipient's speech; Codex progress stays contextual until
a confirmed result is available. The backing model also receives the full context.
`codexVoice.resetAfterOutgoingCall` defaults to true: an outgoing Voice call clears
the resumable context and the next task is prepared empty. Set it to false to allow
an incoming call to continue that outgoing conversation when continuation is enabled.
Idle maintenance archives completed tasks through native Codex after preparing the
next task. The ready task, resumable conversation, active calls and unfinished
delegated work stay open. Only tasks owned by PhoneBridge's journal are selected;
archived conversations remain available in history.

`phone_bridge_status.voiceModels` reads the model catalog from the configured Codex
home using `model/list`, including pagination and each model's advertised reasoning
efforts. The catalog is loaded during idle preparation and cached for 60 seconds;
reconnecting refreshes it. `voiceModelsError` reports discovery failures without
substituting a fixed list. Normal prepared calls use the cached selection.

The default task model is GPT-6.1 Sol/high. `phone_bridge_select_voice` accepts exact model
IDs and supported efforts from that catalog, including newly advertised models,
and validates the combination before changing the task. It updates following
turns; `phone_bridge_restart_voice` prepares a new task and reconnects Voice within
the same phone call with the selected model and reasoning and the original greeting.
The established keypad shortcuts remain `*1<M><R>#` for model/reasoning and
`*0#` for a new task. Accepted commands receive audible feedback. The prior task
remains available as conversation history.

`phone_bridge_hangup` revokes call audio immediately, then confirms CLI session stop,
peer disposal and admission release. A failed transport or media route ends the
original call and retains its outcome. Use `phone.history`, `phone.operation` and
`phone.logs` to inspect it. SIP and Windows audio remain independent of idle CLI
readiness checks.
