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
- Prepare one persistent task per principal and runtime configuration before a
  call when possible. Record creation before sending it and retain the exact task
  acknowledgement. Unknown creation cannot silently create a duplicate. Reopen
  the same task after client/service restart. `keepTaskLoaded` controls whether
  preparation/end leaves it loaded or unsubscribes it.
- Reuse requires the configured home, directory and project and exclusive ownership
  by the original call. Stop a previous session before reusing a recovered task.
  Each call gets a fresh native WebRTC peer and full role-bearing initial prompt.
  Prompt limits reject oversized input explicitly; they never truncate it.
- Task/offer preparation may overlap outgoing SIP ringing. Incoming calls answer
  early with local progress audio. Voice startup follows confirmed SIP connection
  and any required access challenge. The full initial prompt is supplied to
  realtime V3 as role-bearing startup items and to its backing Codex model as
  user-supplied context through realtime start instructions. Native Codex handles
  background task delegation; call context cannot grant permissions or approvals.
  Startup skips workspace/history scanning; the current call's full context is
  already supplied explicitly.
- Each call and generation owns its bounded PCM/Opus queues, DTLS/SRTP transport
  and cleanup. Startup RTP sequence probation excludes isolated transport probes
  before seeding the encrypted replay window. Revoked or suspended authority
  discards audio and codec backlog; it cannot replay into another call.
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
