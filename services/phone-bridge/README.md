# PhoneBridge

PhoneBridge connects SIP calls to Codex Voice through native WebRTC. Codex CLI runs
in the background without a terminal or Desktop app. Voice needs no Micro emulator
or virtual audio cable. Windows calls and announcement screening use the separately
configured `audio` settings. See [Voice requirements](../../specs/PHONE-VOICE.md) and
the [service schema](../../specs/schemas/phone-service.schema.json).

`npm run build:phone` prepares the owning Windows or Linux x64 host's service,
companion, codec and health helper. Linux preparation needs GCC and CMake.
Set `native.executable` to `dist/native/linux-x64/ivy-phone-native` on Linux;
Windows retains `dist/native/phone/Ivy.PhoneRuntime.exe`. Use the executable hash
returned by preparation. Platform and routing requirements are in the Voice spec.

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

The default task model is GPT-6.1 Sol/high. `phone_bridge_update` accepts exact model
IDs and supported efforts from that catalog, including newly advertised models,
and validates the combination before changing the task. Optional `model`,
`reasoningEffort` and `voiceId` preserve omitted values; discover audio voices with
`phone_bridge_speaker_voices`. Audio selection applies to the next Voice session.
Model and reasoning update following
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

Native replay receipts retain at most 4,096 original IDs and 16 MiB of result/error
payloads. Admission reserves a maximum result before invocation, including the
existing per-call cleanup headroom. First completion exchanges that reservation
for the exact boxed payload bytes; replay never consumes another reservation.
Fixed receipt metadata remains bounded by the separate count limit. IPC replies
carry the usage without changing the original result bytes. With no calls or open
Voice work or uncommitted native effect receipts, the service rotates at 4,000
operations or 8 MiB of retained payloads;
the latter leaves room for 128 maximum cleanup results and one further result.
Rotation remains necessary: the pipe has no durable receipt-release acknowledgement
that prevents a delayed duplicate from invoking an evicted operation again.

Journal maintenance runs on its existing worker between commands, in pages of 16
records with coalesced wakes. It sleeps when no eligible work remains. Terminal,
unresolved and archive-confirmation facts are computed when a command changes and
rebuilt from canonical format-8 rows when the exclusive owner opens. Partial call
pages keep original command/sequence lookups and all unresolved receipts fenced.
History usage is maintained transactionally by shared SQLite triggers; eviction
reads one released original at a time and retains its identity hash.

SIP admits at most 16 host response owners and 64 incoming owners, including
unfinished retirement. Each dialog has separate bounded termination delivery.
Four pending remote offers retain their own ACK/timeout identity; further offers
receive 491, and ACKs can retire accepted offers in any order. Four local offers
include their queued work and accepted ACK owners in the same limit. Admission
precedes a codec challenge change. Completed work releases capacity; uncertain
originals and their socket/call fences remain in one host retirement book until
native process exit, without a suspended task per failure. A full host response
budget discards new, unstarted requests before application work; matching call
controls are routed first. SIP-library transport ingress and transaction routing
remain library-owned; these limits cover PhoneBridge's admitted work.

Announcement synthesis hashes and writes the original response in bounded blocks,
up to 10 MiB, before committing its existing one-shot receipt. Native screening
copies and verifies those bytes into a private temporary file before playback.
It decodes PCM on a separate worker with a 32 KiB source window and a 120 ms bounded
prefetch buffer, ready with 60 ms before installation. The audio callback only
copies buffered samples; bridging closes the producer without waiting for disk.
Media disposal waits for that original worker and its private file to close.
The original PCM16 mixing, resampling, repeats and pauses remain unchanged.

To split the current format-7 Voice arrays into format-8 rows, stop the instance
through the owning host CLI and retain a consistent journal backup. Run the exact
prepared PhoneBridge executable with `--upgrade-voice-storage --config ABSOLUTE_PATH`.
This command takes the original exclusive journal ownership, validates and converts
the records atomically, and reports the stored format and row counts. Then deploy
the prepared candidate through the host CLI. Format-7 binaries cannot read the new
representation; retain the backup as recovery evidence rather than rolling back
an older binary over the converted journal.
