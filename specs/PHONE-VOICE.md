# PhoneBridge Voice

Contracts: [service](schemas/phone-service.schema.json),
[native IPC](schemas/phone-ipc.schema.json), [journal](schemas/phone-journal.schema.json).
Implementation: [Codex client](../services/phone-bridge/src/runtime/codex-voice.ts),
[call flow](../services/phone-bridge/src/runtime/flow.ts),
[native audio](../services/phone-bridge/native/phone-runtime/WebRtcAudioRoute.cs).

- Voice uses a hidden Codex CLI app-server client and native WebRTC. It requires no
  Desktop app, terminal, Micro emulator, Windows audio endpoint or virtual cable.
  Windows calls and announcement screening retain their separate audio path.
- `codexVoice` independently selects the executable, exact version/hash, home,
  transport, working directory, optional native project and task configuration.
  Home resolution, daemon startup and Windows shell verification use the same
  shared runtime as AgentManager. Native home instructions, tools and approval
  policy apply; PhoneBridge never grants approvals automatically.
- Prepare one unused persistent task per runtime configuration before a
  call when possible. Record creation before sending it and retain the exact task
  acknowledgement. Unknown creation cannot silently create a duplicate. Reopen
  the same task after client/service restart. `keepTaskLoaded` controls whether
  preparation/end leaves it loaded or unsubscribes it.
  Incoming and MCP calls claim the same unused slot through their original admission.
  Complete a context-free model preparation turn in the background; retain its request
  and native turn identity so reconnects cannot repeat an unknown preparation.
  A call arriving during preparation waits for that same task.
- A conversation consumes its prepared task. After confirmed hangup and cleanup,
  prepare the next unused task in the background; keep completed call/task history.
  A recovered used task must confirm its previous Voice stop before replacement.
  This prevents earlier call instructions from entering a new backing model session.
- `codexVoice.resumeIncomingConversation` optionally continues the last confirmed
  stopped conversation when an admitted incoming caller has the same E.164/SIP
  identity. Keep the exact native task and transfer its latest committed speech
  segments (at most 126 items / 24 KB) into the new realtime session. Context is
  captured during the call, without a summarizing turn or a startup workspace scan.
  The configured incoming initial prompt still runs, including its greeting.
  Outgoing Ivy calls and `*0#` always use fresh tasks. A reset clears the continuation
  pointer; an uncertain stop cannot publish a resumable context. Caller admission
  and any access challenge remain required before voice startup.
- `codexVoice.rememberReasoning` retains a confirmed DTMF/MCP reasoning selection
  across calls and service restarts. It changes the next session's effort while
  retaining the configured default model. Disabling it restores `voiceDefault`.
  An effort unsupported by that default model falls back to its configured effort.
  Without an explicit `voiceDefault`, use GPT-6.1 Sol with `high` reasoning.
- A confirmed missing task or dead native agent loop replaces only its idle cache.
  Earlier call/task identities remain retained. Other errors cannot create another task.
- Reuse requires the configured home, directory and project and exclusive ownership
  by the original call. Stop a previous session before reusing a recovered task.
  A verified loaded task with the same selection skips repeated native reads and
  settings updates while its original control connection remains live. Reconnect,
  unload, errors or a different selection require fresh reconciliation.
  Each call gets a fresh native WebRTC peer and full role-bearing initial prompt.
  Prompt limits reject oversized input explicitly; they never truncate it.
- Task/offer preparation may overlap outgoing SIP ringing. Incoming calls answer
  early without a waiting tone. Voice startup follows confirmed SIP connection
  and any required access challenge. Startup items contain restored history and
  the complete current call assignment. Bound older history against the native
  item/token limit and JSON-encoded backing context without truncating the current
  assignment. The [phone instructions](../instructions/phone-voice.ts) distinguish
  the owner's assignment from the recipient's speech. Send one short connected cue
  after media connects, under the original startup intent; startup history alone
  does not reliably request speech. Acknowledge startup after that cue succeeds;
  an unknown send is never repeated. Also supply the assignment to the backing
  Codex model as user-supplied context through realtime start instructions.
  Route Codex output through the native commentary channel so intermediate work
  remains context and Voice waits for confirmed results. Native Codex handles
  background task delegation; call context cannot grant permissions or approvals.
  Startup skips workspace/history scanning; the current call's full context is
  already supplied explicitly.
- Each call and generation owns its bounded PCM/Opus queues, DTLS/SRTP transport
  and cleanup. `codexVoice.playbackPrebufferMs` controls the PCM startup threshold
  in each direction (default min(60, `queueMs`)); an explicit value cannot exceed
  the queue capacity. `queueMs` bounds the capacity, not constant added latency.
  The reserve rearms after starvation or speech pauses. Startup RTP sequence
  probation excludes isolated transport probes
  before seeding the encrypted replay window. Revoked or suspended authority
  discards audio and codec backlog; it cannot replay into another call.
- Opus uses 48 kHz PCM directly without an identity resampling filter.
  WebRTC transmission uses a dedicated monotonic 20 ms audio clock, with bounded
  catch-up after short scheduling delays and no backlog burst after suspension.
  Realtime queues blend actual PCM cuts over 5 ms within the existing frame;
  continuous samples stay unchanged and authority reset discards blend history.
  Packet gaps preserve decoder/filter state and recover up to 120 ms from
  RTP timestamps with codec FEC/PLC. At the PCM playout deadline, queued following
  packets can supply recovery before the reorder window expires. Longer gaps reset
  the stream instead of replaying old speech. Native media diagnostics include WebRTC receive counters,
  concealed samples and PCM underruns separately from telephone RTP counters.
- Telephone EVS negotiates the highest mutually supported Primary constant rate:
  7.2, 8, 9.6, 13.2, 16.4, 24.4, 32, 48, 64, 96 or 128 kbit/s per channel,
  with NB/WB/SWB/FB bandwidth and 48 kHz PCM. Respect directional rate/bandwidth
  constraints and their valid combinations. Prefer mono Compact packets; offer
  stereo separately with Header-Full-only and independent channel state. The
  mono Voice source is duplicated for stereo transmission; received channels
  are mixed to mono. Send one 20 ms frame-block per packet with the fixed 16 kHz
  RTP clock; accept one to six blocks (20..120 ms). These bounds do not increase
  the configured playback buffer. SC-VBR/DTX, AMR-WB IO and redundancy are not
  offered. Incompatible EVS parameters allow an agreed other codec.
- MCP model selection discovers the configured Codex home's picker-visible models
  through paginated `model/list`, including the reasoning options for each model.
  `phone_bridge_status.voiceModels` exposes the catalog; `voiceModelsError` reports
  failures. Cache discovery for 60 seconds and clear it on reconnect. Idle
  preparation loads it ahead of calls; prepared calls do not wait for a refresh.
  Validate selections against the catalog before settings updates or journal
  intents. Remember arbitrary advertised efforts and only reuse them if the
  configured default model supports them. Keep the established keypad shortcuts.
- A forwarded prompt uses the existing recipient's active call and task, with its
  original operation ID. A model selection affects following task turns. Restart
  prepares a new task and reconnects media in the original SIP call; retained
  generations and operations prevent another restart on replay.
- Hangup revokes SIP/audio before awaiting Codex. Confirm the exact task's realtime
  stop and native peer disposal before releasing the admission. Lost transport
  requires reconnecting to the same runtime for cleanup. Service clients never
  terminate a shared daemon. Uncertain outcomes remain visible in the journal.

Checks: [call flow](../tests/phone-flow.test.ts),
[task lifecycle](../tests/phone-codex-voice.test.ts),
[native WebRTC](../tests/native-phone/WebRtcTests.cs).
