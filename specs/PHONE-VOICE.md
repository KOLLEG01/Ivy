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
  and any required access challenge. The full initial prompt is supplied to
  realtime V3 as role-bearing startup items and to its backing Codex model as
  user-supplied context through realtime start instructions. Native Codex handles
  background task delegation; call context cannot grant permissions or approvals.
  Startup skips workspace/history scanning; the current call's full context is
  already supplied explicitly.
- Each call and generation owns its bounded PCM/Opus queues, DTLS/SRTP transport
  and cleanup. PCM playback uses a 60 ms startup threshold, capped by the configured
  queue size, and rearms after starvation or speech pauses. Startup RTP sequence
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
