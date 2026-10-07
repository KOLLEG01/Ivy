import { setTimeout as delay } from "node:timers/promises";
import { randomUUID } from "node:crypto";
import {
  canonical,
  digest,
  IvyError,
  requireThat,
} from "../../../../packages/sdk/src/node.js";
import type {
  PhoneCall,
  PhoneCallAdmission,
  PhoneScreening,
} from "./admission.js";
import { PhoneCallCommands } from "./calls.js";
import type { PhoneOperation, PhoneJournalIntent } from "./journal.js";
import { phoneOutcomeReservation } from "./journal.js";
import { PhoneNativeClient, validatePhone } from "./native.js";
import type { PhoneStatus } from "./native.js";
import { synthesizeAnnouncement } from "./speech.js";
import type { PhoneSpeechSettings } from "./speech.js";
import type { PhoneCallLogs } from "./call-logs.js";
import { phoneConfigurationSection } from "./settings.js";
import type {
  PhoneCodexVoiceSettings,
  PhoneVoicePort,
  PhoneVoiceSelectionCommand,
} from "./codex-voice.js";
import {
  defaultPhoneVoiceSelection,
  selectedPhoneVoiceModel,
} from "./voice-selection.js";
import type { PhoneVoiceSelection } from "./voice-selection.js";
import type { Agent } from "../../../../packages/sdk/src/node.js";

type PhoneVoiceControl =
  { action: "select"; selection: PhoneVoiceSelection } | { action: "restart" };
export interface PhoneFlowSettings {
  incomingRoute?: "voice" | "windows";
  incomingInitialPrompt?: string;
  voiceDefault?: PhoneVoiceSelection;
  outgoingRoute?: "voice" | "windows";
  accessCodePath?: string | null;
  speech?: PhoneSpeechSettings;
  audio?: Record<string, unknown>;
  codexVoice: PhoneCodexVoiceSettings | null;
  ringSeconds: number;
}
const defaultIncomingInitialPrompt = 'Greet the user with "Hi"';
function result(operation: PhoneOperation): Record<string, unknown> {
  if (!(
    operation.phase === "result" &&
    operation.receipt?.ok &&
    operation.receipt.result !== null &&
    typeof operation.receipt.result === "object" &&
    !Array.isArray(operation.receipt.result)
  ))
    throw new IvyError(
      "phone_command_incomplete",
      "Original Phone command has no confirmed result. Read its retained outcome.",
      "unknown",
    );
  return operation.receipt.result as Record<string, unknown>;
}

/** SIP ownership and cleanup stay in the original call journal; Codex owns only Voice tasks. */
export class PhoneFlow {
  private readonly work = new Map<string, Promise<void>>();
  private readonly admissions = new Set<Promise<PhoneCall>>();
  private readonly cleanups = new Map<string, Promise<void>>();
  private readonly commandSequences = new Map<string, number>();
  private readonly endedCalls = new Set<string>();
  private readonly abandonedCalls = new Set<string>();
  private readonly voiceSelections = new Map<string, PhoneVoiceSelection>();
  private closed = false;
  private closing: Promise<void> | null = null;
  private readonly settings: PhoneFlowSettings;
  constructor(
    readonly calls: PhoneCallCommands,
    private readonly native: PhoneNativeClient,
    settings: PhoneFlowSettings,
    private readonly credentials: () => Promise<{
      username: string | null;
      password: string | null;
    }>,
    private readonly onError: (callId: string, code: string) => void = () =>
      undefined,
    private readonly onConnected: (callId: string) => void = () => undefined,
    private readonly voice: PhoneVoicePort | null = null,
    readonly logs?: PhoneCallLogs,
  ) {
    if (settings.audio) validatePhone("AudioSettings", settings.audio);
    requireThat(
      Number.isInteger(settings.ringSeconds) &&
        settings.ringSeconds >= 1 &&
        settings.ringSeconds <= 120,
      "invalid_arguments",
      "Phone ringing must be bounded.",
    );
    this.settings = structuredClone(settings);
  }
  async prewarmVoice(principalId: string): Promise<void> {
    await this.voice?.prewarm(
      principalId,
      this.defaultVoiceSelection(),
    );
  }
  voiceInputs(principalId: string, callId: string): unknown[] {
    this.original(principalId, callId, "read");
    return this.voice?.inputs?.(callId) ?? [];
  }
  answerVoiceInput(
    principalId: string,
    callId: string,
    id: Agent.RequestId,
    reply: Agent.Reply,
  ): void {
    const call = this.original(principalId, callId, "connect");
    this.continuing(call);
    requireThat(
      this.voice?.answer,
      "phone_voice_unavailable",
      "Configured Voice cannot receive task inputs.",
    );
    this.voice.answer(callId, id, reply);
  }
  private windowsAudio(): Record<string, unknown> {
    requireThat(
      this.settings.audio,
      "phone_audio_unconfigured",
      "Windows calls require explicit Windows audio settings.",
    );
    return structuredClone(this.settings.audio);
  }
  audioProfiles() {
    return this.settings.audio
      ? [{ route: "windows", settings: this.windowsAudio() }]
      : [];
  }
  configuration() {
    return {
      incomingRoute: this.settings.incomingRoute ?? "voice",
      outgoingRoute: this.settings.outgoingRoute ?? "windows",
      screeningConfigured: !!this.settings.speech,
      accessChallengeConfigured:
        !!this.settings.accessCodePath &&
        !!this.calls.admission.definition.challengedIncoming?.length,
      directDialConfigured:
        this.calls.admission.definition.allowDirectDial === true,
      voiceDefault: this.settings.voiceDefault ?? defaultPhoneVoiceSelection,
      voiceEffectiveDefault: this.defaultVoiceSelection(),
      voicePlaybackPrebufferMs: this.settings.codexVoice
        ? (this.settings.codexVoice.playbackPrebufferMs ?? Math.min(60, this.settings.codexVoice.queueMs ?? 80)) : null,
      voiceQueueMs: this.settings.codexVoice?.queueMs ?? null,
      rememberVoiceReasoning: this.settings.codexVoice?.rememberReasoning ?? false,
      resumeIncomingConversation: this.settings.codexVoice?.resumeIncomingConversation ?? false,
    };
  }

  request(
    principalId: string,
    operationId: string,
    recipientId: string | null,
    route?: "voice" | "windows",
    destination?: string,
    voicePrompt?: string,
  ): Promise<PhoneCall> {
    const selectedRoute = route ?? this.settings.outgoingRoute ?? "windows";
    const forwarded = this.calls.journal.get(operationId);
    if (forwarded) {
      requireThat(
        forwarded.intent.method === "call.forwardVoice" &&
          forwarded.intent.callId,
        "mutation_conflict",
        "Phone request belongs to another command.",
      );
      const admission = this.calls.admission.outgoing(
        forwarded.intent.epoch,
        operationId,
        principalId,
        recipientId,
        selectedRoute,
        undefined,
        destination,
        voicePrompt,
      );
      requireThat(
        forwarded.intent.requestHash === digest(canonical(admission)),
        "mutation_conflict",
        "Original forwarded Phone request changed.",
      );
      const call = this.original(principalId, forwarded.intent.callId, "read");
      return this.admit(() => this.forwardVoiceRequest(call, admission));
    }
    if (selectedRoute === "voice") {
      requireThat(
        this.voice && this.settings.codexVoice,
        "phone_voice_unconfigured",
        "Voice calls require the configured Codex CLI runtime.",
      );
      if (
        voicePrompt &&
        recipientId &&
        !this.calls.journal.callForOperation(operationId)
      ) {
        const current = this.calls.journal
          .currentCalls()
          .find(
            (call) =>
              this.callRoute(call) === "voice" &&
              this.calls.admission.matchesRecipient(call, recipientId),
          );
        if (current) {
          const admission = this.calls.admission.outgoing(
            this.native.epoch,
            operationId,
            principalId,
            recipientId,
            selectedRoute,
            undefined,
            destination,
            voicePrompt,
          );
          return this.admit(() => this.forwardVoiceRequest(current, admission));
        }
      }
    } else this.windowsAudio();
    return this.admit(() =>
      this.calls.admitOutgoing(
        principalId,
        operationId,
        recipientId,
        selectedRoute,
        undefined,
        destination,
        voicePrompt,
      ),
    );
  }
  private async forwardVoiceRequest(
    call: PhoneCall,
    admission: PhoneCallAdmission,
  ): Promise<PhoneCall> {
    while (this.work.has(call.callId))
      await this.work.get(call.callId)!.catch(() => undefined);
    const requestHash = digest(canonical(admission)),
      prior = this.calls.journal.get(admission.operationId);
    if (prior) {
      requireThat(
        prior.intent.method === "call.forwardVoice" &&
          prior.intent.callId === call.callId &&
          prior.intent.requestHash === requestHash,
        "mutation_conflict",
        "Original forwarded Voice request changed.",
      );
      if (
        prior.phase === "result" &&
        prior.receipt?.ok &&
        (prior.receipt.result as { state?: string })?.state === "sent"
      )
        return call;
      throw new IvyError(
        "phone_voice_prompt_unknown",
        "Original forwarded Voice prompt remains unknown and cannot be repeated.",
        "unknown",
      );
    }
    this.continuing(call);
    const pending = Promise.resolve()
      .then(async () => {
        this.continuing(call);
        const generation = this.calls.journal.latestVoiceGeneration(
            call.callId,
          ),
          threadId =
            generation === null
              ? null
              : this.calls.journal.voiceTask(call.callId, generation);
        requireThat(
          this.voice && threadId,
          "phone_voice_task_missing",
          "Current call has no bound Voice task.",
        );
        await this.voice.prompt(
          call,
          threadId,
          admission.voicePrompt!,
          admission.operationId,
          requestHash,
          () => this.confirmConnectedForVoice(call),
        );
      })
      .finally(() => this.work.delete(call.callId));
    this.work.set(call.callId, pending);
    await pending;
    return call;
  }
  private original(
    principalId: string,
    callId: string,
    action: "read" | "connect" | "cleanup",
  ): PhoneCall {
    const call = this.calls.journal.getCall(callId);
    requireThat(
      call,
      "phone_call_missing",
      "Original Phone admission is unavailable.",
    );
    this.calls.admission.authorize(principalId, call, action);
    return call;
  }
  private continuing(call: PhoneCall): void {
    requireThat(
      !this.closed &&
        !this.endedCalls.has(call.callId) &&
        this.calls.journal.currentCall(call.callId)?.callId === call.callId &&
        call.epoch === this.native.epoch &&
        !this.calls.journal.callCommand(call.callId, "call.hangup") &&
        !this.calls.journal.callCommand(call.callId, "call.release"),
      "phone_call_cancelled",
      "The original call is no longer admitted for positive work.",
    );
    this.calls.admission.authorize(call.principalId, call, "connect");
  }
  screen(
    principalId: string,
    operationId: string,
    recipientId: string,
    screening: PhoneScreening,
  ): Promise<PhoneCall> {
    this.windowsAudio();
    requireThat(
      this.settings.speech,
      "phone_speech_unconfigured",
      "Screening requires configured speech synthesis.",
    );
    return this.admit(() =>
      this.calls.admitOutgoing(
        principalId,
        operationId,
        recipientId,
        "windows",
        screening,
      ),
    );
  }
  async bridgeScreening(
    principalId: string,
    callId: string,
  ): Promise<PhoneCall> {
    const call = this.original(principalId, callId, "connect");
    this.continuing(call);
    requireThat(
      call.screening,
      "phone_screening_missing",
      "Original call is not a screening session.",
    );
    // A caller may poll/attempt bridging before the recipient answers. Refuse that request
    // before reserving the one original bridge effect, so acceptance can still be bridged.
    const observed = await this.native.observe(callId);
    this.continuing(call);
    requireThat(
      observed.call?.id === callId &&
        observed.call.state === "connected" &&
        ["accepted", "bridged"].includes(
          observed.call.features?.screening ?? "",
        ),
      "phone_screening_not_accepted",
      "The original screening recipient has not accepted.",
    );
    try {
      result(
        await this.calls.bridgeScreening(
          principalId,
          callId,
          this.windowsAudio(),
        ),
      );
    } catch (error) {
      // A failed/uncertain device attachment must not leave an accepted, silent SIP call open.
      await this.cleanup(principalId, callId);
      throw error;
    }
    return call;
  }
  voiceSelection(callId: string): PhoneVoiceSelection {
    return this.voiceSelections.get(callId) ?? this.defaultVoiceSelection();
  }
  private defaultVoiceSelection(): PhoneVoiceSelection {
    const selection = this.settings.voiceDefault ?? defaultPhoneVoiceSelection;
    const effort = this.settings.codexVoice?.rememberReasoning
      ? this.calls.journal.rememberedVoiceReasoning() : null;
    return effort && !(selection.model === 'gpt-6-luna' && effort === 'ultra')
      ? { ...selection, reasoningEffort: effort } : selection;
  }
  callRoute(call: PhoneCall): "voice" | "windows" {
    return (
      call.route ??
      (call.direction === "incoming"
        ? (this.settings.incomingRoute ?? "voice")
        : (this.settings.outgoingRoute ?? "windows"))
    );
  }
  accept(
    principalId: string,
    operationId: string,
    callId: string,
  ): Promise<PhoneCall> {
    return this.admit(() =>
      this.calls.admitIncoming(
        principalId,
        operationId,
        callId,
        this.settings.incomingRoute,
      ),
    );
  }
  private admit(action: () => Promise<PhoneCall>): Promise<PhoneCall> {
    requireThat(!this.closed, "service_not_ready", "Phone is stopping.");
    const pending = action()
      .then(async (call) => {
        if (this.closed) {
          await this.cleanup(call.principalId, call.callId);
          throw new IvyError(
            "service_not_ready",
            "Phone stopped during original admission.",
          );
        }
        this.schedule(call);
        return call;
      })
      .finally(() => this.admissions.delete(pending));
    this.admissions.add(pending);
    return pending;
  }
  private schedule(call: PhoneCall): void {
    call = structuredClone(call);
    if (
      this.work.has(call.callId) ||
      call.epoch !== this.native.epoch ||
      this.calls.journal.currentCall(call.callId)?.callId !== call.callId
    )
      return;
    if (
      this.calls.journal.callCommand(
        call.callId,
        call.direction === "outgoing" ? "call.dial" : "call.answer",
      )
    )
      return;
    const pending = Promise.resolve()
      .then(() => this.connect(call))
      .catch(async (error) => {
        this.abandonedCalls.add(call.callId);
        try {
          this.onError(call.callId, IvyError.from(error).code);
        } catch {
          /* Diagnostics cannot bypass cleanup. */
        }
        try {
          await this.cleanupWhenObserved(call);
        } catch (cleanupError) {
          try {
            this.onError(call.callId, IvyError.from(cleanupError).code);
          } catch {}
        }
      })
      .finally(() => this.work.delete(call.callId));
    this.work.set(call.callId, pending);
  }
  private async cleanupWhenObserved(
    call: PhoneCall,
    observed?: PhoneStatus,
  ): Promise<void> {
    if (!this.calls.journal.currentCall(call.callId)) {
      this.abandonedCalls.delete(call.callId);
      return;
    }
    const status = observed ?? (await this.native.observe(call.callId));
    if (status.call?.id !== call.callId) {
      const created = this.calls.journal.callCommand(
        call.callId,
        call.direction === "incoming" ? "call.claim" : "call.prepare",
      );
      if (
        !created ||
        (created.phase === "result" && created.receipt?.ok === false)
      ) {
        this.calls.cancelUnprepared(call.principalId, call.callId);
        this.abandonedCalls.delete(call.callId);
      }
      // A timed-out creation may still complete. Never spend the one original
      // hangup against a call that the native owner has not observed yet.
      return;
    }
    await this.cleanup(call.principalId, call.callId);
    this.abandonedCalls.delete(call.callId);
  }
  private async confirmConnectedForVoice(call: PhoneCall): Promise<void> {
    // The first native status immediately after SIP answer can be transiently
    // incomplete. Only a failed read is retried; a confirmed ended call is final.
    for (let attempt = 0; attempt < 3; attempt++) {
      this.continuing(call);
      let observed: PhoneStatus;
      try {
        observed = await this.native.observe(call.callId);
      } catch (error) {
        if (IvyError.from(error).code !== "invalid_arguments") throw error;
        if (attempt === 2)
          throw new IvyError(
            "phone_voice_status_invalid",
            "Native call status remained invalid after SIP answer.",
          );
        await delay(100 * (attempt + 1));
        continue;
      }
      this.continuing(call);
      requireThat(
        observed.call?.id === call.callId &&
          observed.call.state === "connected",
        "phone_call_cancelled",
        "Original call ended before Voice submission.",
      );
      return;
    }
  }
  private initialVoicePrompt(call: PhoneCall): string {
    const greeting =
      call.direction === "incoming"
        ? (this.settings.incomingInitialPrompt ?? defaultIncomingInitialPrompt)
        : "Greet the person you called.";
    return call.voicePrompt ?? greeting;
  }
  private callResult(
    call: PhoneCall,
    operation: PhoneOperation,
    states: string[],
  ): void {
    const observed = result(operation);
    validatePhone("SipCallObservation", observed);
    if (!(
      observed["id"] === call.callId &&
      observed["direction"] === call.direction &&
      states.includes(String(observed["state"]))
    ))
      throw new IvyError(
        "phone_call_unconfirmed",
        "The original SIP call did not confirm the required state.",
        "unknown",
      );
  }
  private async commandFeedback(
    call: PhoneCall,
    commandSequence: number,
    success: boolean,
  ): Promise<void> {
    try {
      this.continuing(call);
      result(
        await this.calls.feedback(
          call.principalId,
          call.callId,
          commandSequence,
          success,
        ),
      );
    } catch (error) {
      try {
        this.onError(call.callId, IvyError.from(error).code);
      } catch {}
    }
  }
  private scheduleSelection(
    call: PhoneCall,
    commandSequence: number,
    selection: PhoneVoiceSelection,
  ): void {
    if (this.work.has(call.callId)) return;
    const pending = this.applySelection(call, { commandSequence }, selection)
      .then(
        () => this.commandFeedback(call, commandSequence, true),
        async (error) => {
          try {
            this.onError(call.callId, IvyError.from(error).code);
          } catch {}
          await this.commandFeedback(call, commandSequence, false);
        },
      )
      .finally(() => this.work.delete(call.callId));
    this.work.set(call.callId, pending);
  }
  async controlVoice(
    principalId: string,
    callId: string,
    operationId: string,
    control: PhoneVoiceControl,
  ): Promise<PhoneOperation> {
    const call = this.original(principalId, callId, "read");
    const prior = this.calls.journal.get(operationId);
    if (prior) {
      requireThat(
        prior.intent.callId === callId &&
          (control.action === "select"
            ? prior.intent.method === "call.selectVoice" &&
              prior.intent.model === control.selection.model &&
              prior.intent.reasoningEffort === control.selection.reasoningEffort
            : prior.intent.method === "call.restartVoice"),
        "mutation_conflict",
        "Voice operation belongs to another command or selection.",
      );
      return prior;
    }
    this.continuing(call);
    requireThat(
      this.callRoute(call) === "voice",
      "phone_voice_task_missing",
      "Voice controls require a Voice call.",
    );
    requireThat(
      !this.work.has(callId),
      "phone_call_busy",
      "Wait for the current call action before changing Voice.",
    );
    // Reserve the same per-call slot used by setup and keypad commands before any await.
    const pending = Promise.resolve()
      .then(() =>
        control.action === "select"
          ? this.applySelection(call, { operationId }, control.selection)
          : this.recycle(call, operationId),
      )
      .finally(() => this.work.delete(callId));
    this.work.set(callId, pending);
    await pending;
    const operation = this.calls.journal.get(operationId);
    requireThat(
      operation,
      "phone_operation_missing",
      "Voice command has no retained outcome.",
    );
    return operation;
  }
  private scheduleRecycle(call: PhoneCall, commandSequence: number): void {
    if (this.work.has(call.callId)) return;
    const pending = this.recycle(call)
      .then(
        () => this.commandFeedback(call, commandSequence, true),
        async (error) => {
          try {
            this.onError(call.callId, IvyError.from(error).code);
          } catch {}
          await this.commandFeedback(call, commandSequence, false);
        },
      )
      .finally(() => this.work.delete(call.callId));
    this.work.set(call.callId, pending);
  }
  async hangup(principalId: string, callId: string): Promise<void> {
    this.original(principalId, callId, "cleanup");
    await this.cleanup(principalId, callId);
  }
  private async prepareVoice(
    call: PhoneCall,
    generation = 0,
  ): Promise<{ threadId: string; sdp: string }> {
    this.continuing(call);
    requireThat(
      this.voice && this.settings.codexVoice,
      "phone_voice_unconfigured",
      "Voice requires its Codex runtime.",
    );
    const selection = this.voiceSelection(call.callId);
    this.voiceSelections.set(call.callId, selection);
    const [task, media] = await Promise.allSettled([
      this.voice.prepare(call, generation, selection),
      this.calls.prepareRealtime(
        call.principalId,
        call.callId,
        generation,
        this.settings.codexVoice.queueMs ?? 80,
        this.settings.codexVoice.playbackPrebufferMs,
      ),
    ]);
    if (task.status === "rejected") throw task.reason;
    if (media.status === "rejected") throw media.reason;
    const offer = result(media.value);
    requireThat(
      offer["callId"] === call.callId &&
        offer["generation"] === generation &&
        typeof offer["sdp"] === "string",
      "phone_voice_sdp_invalid",
      "Native WebRTC offer belongs to another call or generation.",
    );
    this.continuing(call);
    return { threadId: task.value, sdp: offer["sdp"] };
  }
  private async startVoice(
    call: PhoneCall,
    prepared: { threadId: string; sdp: string },
    generation = 0,
  ): Promise<void> {
    await this.confirmConnectedForVoice(call);
    const answer = await this.voice!.start(
      call,
      generation,
      prepared.threadId,
      this.initialVoicePrompt(call),
      prepared.sdp,
      () => this.continuing(call),
    );
    this.continuing(call);
    const connected = result(
      await this.calls.answerRealtime(
        call.principalId,
        call.callId,
        generation,
        answer,
      ),
    );
    requireThat(
      connected["callId"] === call.callId && connected["connected"] === true,
      "phone_voice_not_ready",
      "Native Voice media did not connect to the original call.",
    );
    this.continuing(call);
    await this.voice!.connected?.(call.callId, () => this.confirmConnectedForVoice(call));
  }
  private async connect(call: PhoneCall): Promise<void> {
    this.continuing(call);
    const usage = this.calls.journal.status();
    requireThat(
      usage.operations + 16 <= this.calls.journal.limits.maxOperations &&
        usage.reservedBytes + 16 * phoneOutcomeReservation <=
          this.calls.journal.limits.maxBytes,
      "phone_journal_capacity",
      "Phone requires room for the complete original call and cleanup before starting.",
    );
    if (call.direction === "outgoing")
      this.callResult(
        call,
        await this.calls.prepare(call.principalId, call.callId),
        ["prepared"],
      );
    else
      this.callResult(
        call,
        await this.calls.claim(call.principalId, call.callId),
        ["ringing"],
      );
    const challenged =
      call.direction === "incoming" &&
      this.calls.admission.challengesIncoming(call.incoming!);
    let challenge: Record<string, unknown> | null = null;
    if (challenged) {
      requireThat(
        this.settings.accessCodePath,
        "phone_access_unconfigured",
        "Incoming challenge requires protected code settings.",
      );
      challenge = (await phoneConfigurationSection(
        this.settings.accessCodePath,
        "access",
      )) as Record<string, unknown>;
      validatePhone("CallAccessSettings", challenge);
    }
    result(await this.calls.features(call.principalId, call.callId, challenge));
    if (call.screening) {
      requireThat(
        this.settings.speech,
        "phone_speech_unconfigured",
        "Screening requires speech settings.",
      );
      const announcement = await synthesizeAnnouncement(
        this.calls.journal,
        call,
        call.screening.announcement,
        this.settings.speech,
      );
      this.continuing(call);
      result(
        await this.calls.prepareScreening(call.principalId, call.callId, {
          ...announcement,
          timeoutSeconds: call.screening.timeoutSeconds,
          repeatCount: call.screening.repeatCount,
        }),
      );
    }
    const route = this.callRoute(call),
      prepareDuringDial = !call.screening && !challenged && route === "voice";
    let prepared: { threadId: string; sdp: string } | null = null;
    if (call.direction === "incoming") {
      this.continuing(call);
      this.callResult(
        call,
        await this.calls.answer(call.principalId, call.callId),
        ["connected"],
      );
    } else {
      const credentials = await this.credentials();
      this.continuing(call);
      // Preparation is cached before a call when possible and otherwise overlaps ringing.
      // Voice inference starts only after the original SIP connection is confirmed.
      const [dialed, voice] = await Promise.allSettled([
        this.calls.dial(
          call.principalId,
          call.callId,
          credentials,
          this.settings.ringSeconds,
        ),
        prepareDuringDial ? this.prepareVoice(call) : Promise.resolve(null),
      ]);
      if (dialed.status === "rejected") throw dialed.reason;
      this.callResult(call, dialed.value, ["connected"]);
      if (voice.status === "rejected") throw voice.reason;
      prepared = voice.value;
    }
    try {
      this.onConnected(call.callId);
    } catch {}
    if (call.screening) return;
    if (challenged) {
      const deadline =
        performance.now() +
        (Number(challenge!["timeoutSeconds"] ?? 60) + 5) * 1000;
      while (true) {
        this.continuing(call);
        const observed = await this.native.observe(call.callId);
        requireThat(
          observed.call?.id === call.callId &&
            observed.call.state === "connected",
          "phone_access_failed",
          "Original challenge call ended.",
        );
        if (observed.call.features?.access === "authenticated") break;
        requireThat(
          observed.call.features?.access === "awaiting_code" &&
            performance.now() < deadline,
          "phone_access_failed",
          "Original access challenge did not authenticate.",
        );
        await delay(100);
      }
      result(await this.calls.upgradeCodec(call.principalId, call.callId));
    }
    if (route === "windows") {
      result(
        await this.calls.connectWindows(
          call.principalId,
          call.callId,
          this.windowsAudio(),
        ),
      );
      return;
    }
    prepared ??= await this.prepareVoice(call);
    await this.startVoice(call, prepared);
  }

  async observe(observed?: PhoneStatus): Promise<void> {
    for (const call of this.calls.journal.currentCalls()) {
      if (call.epoch !== this.native.epoch) continue;
      try {
        const status =
          observed?.call?.id === call.callId
            ? observed
            : await this.native.observe(call.callId);
        try {
          this.logs?.record(call, status);
        } catch {}
        const ended =
          status.call?.id === call.callId &&
          (["local_ended", "outcome_unknown"].includes(status.call.state) ||
            status.media?.audio?.state === "revoked" ||
            status.media?.failed === true ||
            this.voice?.failed(call.callId));
        if (ended) this.endedCalls.add(call.callId);
        if (this.work.has(call.callId)) continue;
        if (ended || this.abandonedCalls.has(call.callId)) {
          await this.cleanupWhenObserved(call, status);
          continue;
        }
        if (
          status.call?.id !== call.callId ||
          status.call.state !== "connected"
        )
          continue;
        const previous = this.commandSequences.get(call.callId) ?? 0;
        const next = status.call.features?.commands.find(
          (item) => item.sequence > previous,
        );
        if (!next) continue;
        if (next.sequence !== previous + 1)
          this.onError(call.callId, "phone_command_overflow");
        this.commandSequences.set(call.callId, next.sequence);
        if (next.command === "new_voice")
          this.scheduleRecycle(call, next.sequence);
        else {
          requireThat(
            next.model && next.reasoningEffort,
            "phone_voice_selection_invalid",
            "Native Voice selection is incomplete.",
          );
          this.scheduleSelection(
            call,
            next.sequence,
            selectedPhoneVoiceModel(next.model, next.reasoningEffort),
          );
        }
      } catch (error) {
        try {
          this.onError(call.callId, IvyError.from(error).code);
        } catch {}
      }
    }
  }
  private async applySelection(
    call: PhoneCall,
    command: PhoneVoiceSelectionCommand,
    selection: PhoneVoiceSelection,
  ): Promise<void> {
    await this.confirmConnectedForVoice(call);
    const generation = this.calls.journal.latestVoiceGeneration(call.callId),
      threadId =
        generation === null
          ? null
          : this.calls.journal.voiceTask(call.callId, generation);
    requireThat(
      this.voice && threadId,
      "phone_voice_task_missing",
      "Current Voice task is unavailable.",
    );
    await this.voice.select(call, threadId, command, selection, () =>
      this.confirmConnectedForVoice(call),
    );
    if (this.settings.codexVoice?.rememberReasoning)
      this.calls.journal.rememberVoiceReasoning(selection.reasoningEffort);
    this.continuing(call);
    this.voiceSelections.set(call.callId, selection);
  }
  private async recycle(call: PhoneCall, operationId?: string): Promise<void> {
    await this.confirmConnectedForVoice(call);
    const generation = this.calls.journal.latestVoiceGeneration(call.callId);
    requireThat(
      this.voice && generation !== null && generation < 128,
      "phone_voice_generation_exhausted",
      "Current Voice generation cannot be restarted.",
    );
    const next = generation + 1,
      selection = this.voiceSelection(call.callId);
    const intent:
      (PhoneJournalIntent & { method: "call.restartVoice" }) | null =
      operationId
        ? {
            epoch: call.epoch,
            callId: call.callId,
            operationId,
            method: "call.restartVoice",
            voiceGeneration: next,
            ...selection,
            requestHash: digest(
              canonical({ voiceGeneration: next, ...selection }),
            ),
          }
        : null;
    if (intent) this.calls.journal.submit(intent);
    try {
      // A new task is prepared while the current conversation remains connected.
      const threadId = await this.voice.prepare(call, next, selection);
      this.continuing(call);
      await this.voice.stop(call.callId, threadId);
      result(
        await this.calls.stopRealtime(
          call.principalId,
          call.callId,
          generation,
        ),
      );
      this.continuing(call);
      const offer = result(
        await this.calls.prepareRealtime(
          call.principalId,
          call.callId,
          next,
          this.settings.codexVoice?.queueMs ?? 80,
          this.settings.codexVoice?.playbackPrebufferMs,
        ),
      );
      requireThat(
        typeof offer["sdp"] === "string",
        "phone_voice_sdp_invalid",
        "Native restart has no SDP offer.",
      );
      await this.startVoice(call, { threadId, sdp: offer["sdp"] }, next);
      if (intent) this.calls.journal.finishVoiceRestart(intent, threadId);
    } catch (error) {
      if (
        intent &&
        this.calls.journal.epoch === call.epoch &&
        this.calls.journal.get(intent.operationId)?.phase === "submitted"
      )
        this.calls.journal.finishVoiceRestart(intent, null);
      // A partial restart must not leave a connected SIP call with no Voice owner.
      await this.cleanupWhenObserved(call).catch(() => undefined);
      throw error;
    }
  }
  private cleanup(principalId: string, callId: string): Promise<void> {
    this.original(principalId, callId, "cleanup");
    const prior = this.cleanups.get(callId);
    if (prior) return prior;
    this.endedCalls.add(callId);
    const pending = this.cleanupCore(principalId, callId).finally(() =>
      this.cleanups.delete(callId),
    );
    this.cleanups.set(callId, pending);
    return pending;
  }
  private async cleanupCore(
    principalId: string,
    callId: string,
  ): Promise<void> {
    const call = this.original(principalId, callId, "cleanup");
    if (
      call.epoch !== this.native.epoch ||
      this.calls.journal.currentCall(callId)?.callId !== callId
    )
      return;
    const created = this.calls.journal.callCommand(
      callId,
      call.direction === "incoming" ? "call.claim" : "call.prepare",
    );
    if (
      !created ||
      (created.phase === "result" && created.receipt?.ok === false)
    ) {
      this.calls.cancelUnprepared(principalId, callId);
      return;
    }
    // Revoke live audio immediately; pending task startup cannot keep the caller connected.
    this.callResult(call, await this.calls.hangup(principalId, callId), [
      "local_ended",
      "outcome_unknown",
    ]);
    await this.voice?.stop(callId);
    // Even a failed task preparation may already own an SDP/peer. Clean its exact generation.
    for (let generation = 128; generation >= 0; generation--) {
      const prepared = this.calls.journal.callCommand(
        callId,
        "call.realtime.prepare",
        generation,
      );
      if (!prepared) continue;
      if (prepared.phase === "result" && prepared.receipt?.ok === false) break;
      result(await this.calls.stopRealtime(principalId, callId, generation));
      break;
    }
    result(await this.calls.release(principalId, callId));
    this.commandSequences.delete(callId);
    this.endedCalls.delete(callId);
    this.abandonedCalls.delete(callId);
    this.voiceSelections.delete(callId);
    if (!this.closed && this.callRoute(call) === "voice")
      void this.prewarmVoice(principalId).catch((error) =>
        this.onError(callId, IvyError.from(error).code),
      );
  }
  close(): Promise<void> {
    if (this.closing) return this.closing;
    this.closed = true;
    return (this.closing = (async () => {
      await Promise.allSettled(this.admissions);
      try {
        const outcomes = await Promise.allSettled(
          this.calls.journal
            .currentCalls()
            .map((call) => this.cleanup(call.principalId, call.callId)),
        );
        const failed = outcomes.find((value) => value.status === "rejected");
        if (failed?.status === "rejected") throw failed.reason;
      } finally {
        await Promise.allSettled(this.work.values());
        await this.voice?.close();
      }
    })());
  }
}
