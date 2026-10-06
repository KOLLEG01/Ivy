# PhoneBridge Voice input

Contracts: [Micro](schemas/phone-micro.schema.json),
[Voice lifetime](schemas/phone-voice-lifetime.schema.json),
[task binding](schemas/phone-voice-archive.schema.json).
Implementation: [native runtime](../services/phone-bridge/native/phone-runtime),
[Voice controller](../services/phone-bridge/src/runtime/desktop-voice-controller.ts).

- Emulated Micro over loopback USB/IP controls Voice through ordinary Desktop HID.
  Driver setup is separate; no Desktop hooks, automatic elevation or driver-security changes.
- Input requires the original live connection, completed handshake and pending read.
  Do not queue input for a future connection or carry events across generations.
- Codex Desktop 26.917 in Realtime Micro mode starts Voice on the first press but
  ends a short single tap at its 350 ms deadline. PhoneBridge sends two short taps
  within that deadline to latch Voice; a long press ends it. Both taps must use the
  same original HID generation. Only the Desktop capture confirms Voice startup.
- Initial startup prewarms the caller's last positively bound, unarchived local chat,
  including a Desktop-created chat promoted to an ordinary task. After Micro starts
  Voice, PhoneBridge transfers its single new startup chat to that exact previous chat.
  Original call receipts prove the target across process restarts; the saved Desktop
  reference alone cannot prove that Voice resumed it. With no previous usable chat,
  PhoneBridge adopts the new Desktop chat. It never creates an internal-project task.
  Ambiguous or unavailable ownership fails closed. The reused chat and its conversation
  remain unarchived in Recents. The confirmed transient startup chat enters the existing
  archive worker; the worker never deletes it or archives the reused conversation.
  If Desktop keeps definitively rejecting the reuse transfer, adopt the single
  original startup chat instead of ending the call. Keep both chats and their
  history; ambiguous or unknown transfer outcomes never permit this fallback.
- Prompt delivery and archival use that retained generation. After each incoming SIP
  connection, send `incomingInitialPrompt` to the bound Desktop chat only after its
  rollout records a fresh active realtime session. Transfer acknowledgement and task
  readiness can precede that session; a previous session cannot satisfy the check.
  Missing realtime readiness suppresses the greeting without ending an already
  bound call; the conversation remains available.
  A confirmed local task may still be `notLoaded` while its realtime session is
  active. Do not wait for task loading before binding or prompt delivery; sending
  the complete prompt starts its task turn and preserves the conversation context.
  The default is `Greet the user with "Hi"`. Outgoing
  calls keep their greeting followed by their configured `voicePrompt`. Binding
  follows an early incoming answer. The caller hears locally generated progress audio
  while Desktop audio and the task prepare; caller audio cannot enter Voice until the
  original task is bound. Outgoing Voice calls use the same signal after answer. The
  progress audio ends before prompt submission so its spoken reply can reach the
  caller. Journal before mutation; unknown outcomes never authorize another send.
- Outgoing Voice requests with a prompt reuse an admitted incoming or outgoing
  Voice call to the same configured recipient. Wait for current call setup or task
  control, then send the prompt unchanged to the latest bound task without dialing
  or adding a greeting. Return the existing call identity. Retain the send under
  the request's operation ID and original admission arguments; a retry returns its
  confirmed result or unknown outcome without another send, including after the
  call ends. Windows and screening calls retain their existing admission behavior.
- The default for a new PhoneBridge task is GPT-6 Sol / high; `voiceDefault` may
  configure a different supported model and reasoning selection. Authenticated
  `*1<M><R>#` updates the current call selection without restarting
  its task: M=1 Luna, 2 Sol, 3 Astra; R=1 low, 2 medium, 3 high, 4 xhigh, 5 max,
  6 ultra (not Luna). PhoneBridge submits one journaled App Tools follow-up with
  the model and reasoning override to the bound task, so its following turns use
  the selection without a Voice restart. A lost acknowledgement is never retried.
- Authenticated `*0#` retains the SIP/audio owner and the live Voice session while it
  creates a local projectless PhoneBridge task with the current model selection. App Tools transfers
  that session to the exact new task before PhoneBridge sends the configured greeting
  prompt again. Earlier created tasks remain available as conversation history and can
  be archived after Voice ends. The native call retains both commands when they arrive
  between service polls. Completed commands receive distinct
  local success or failure audio feedback. Phone status exposes the default and selection.
- Uncertain Voice stop retains the call slot until capture proves no owner. A transport
  receipt alone cannot prove Voice start, task ownership or successful shutdown.
- A failed WASAPI or mute route may keep the original SIP dialog briefly with silent
  audio. One journaled rebind may dispose and replace only that call's route, after
  Desktop process, original capture and bound task are reconfirmed. A second failure,
  unclear owner or unconfirmed resource disposal ends the call without another rebind.
- Optional hotkey fallback is disabled by default. Only confirmed non-submission and a fresh
  idle baseline permit it; partial/unknown gestures cannot be replayed through another method.
- App Tools discovery may reconcile an unknown attachment; it cannot authorize another
  attachment, replacement device or AgentManager-based Voice lifecycle.
- Each new App Tools connection uses the newest complete local bundled release. Legacy
  file paths and hashes do not pin that release. Tool contracts and the exact local
  controller/task identity remain checked; a failure is a persistent Phone diagnostic
  until a later idle probe succeeds.
- Replaced PhoneBridge-created tasks are archived through Desktop App Tools and remain
  available in Desktop's archive. PhoneBridge never deletes these tasks or calls
  AgentManager; all task control uses the existing Desktop connection.

Checks: [Voice tasks](../tests/phone-voice-tasks.test.ts),
[native Voice](../tests/native-phone/DesktopVoiceSessionTests.cs).
