import { setTimeout as delay } from "node:timers/promises";
import { canonical, digest, IvyError, requireThat } from "../../../../packages/sdk/src/node.js";
import type { PhoneCall, PhoneCallAdmission, PhoneScreening } from "./admission.js";
import { PhoneCallCommands } from "./calls.js";
import type { PhoneCallTarget, PhoneOperation } from "./journal.js";
import type { PhoneJournalIntent } from "./journal.js";
import { phoneOutcomeReservation } from "./journal.js";
import { PhoneNativeClient, validatePhone } from "./native.js";
import type { PhoneStatus } from "./native.js";
import type { PhoneDesktopObservation } from "./native.js";
import { PhoneVoiceArchive } from "./desktop-voice-archive.js";
import type { PhoneVoiceArchiveSettings } from "./desktop-voice-archive.js";
import { synthesizeAnnouncement } from "./speech.js";
import type { PhoneSpeechSettings } from "./speech.js";
import type { PhoneCallLogs } from "./call-logs.js";
import { phoneConfigurationSection } from "./settings.js";
import { PhoneVoiceTasks } from "./desktop-voice-tasks.js";
import type { PhoneVoiceSelectionCommand } from "./desktop-voice-tasks.js";
import { PhoneVoiceController } from "./desktop-voice-controller.js";
import { PhoneAppToolsPool } from "./desktop-app-tools-pool.js";
import { readDesktopVoiceReference } from "./desktop-voice-inventory.js";
import { defaultPhoneVoiceSelection, selectedPhoneVoiceModel } from "./voice-selection.js";
import type { PhoneVoiceSelection } from "./voice-selection.js";

type PhoneVoiceControl = { action: 'select'; selection: PhoneVoiceSelection } | { action: 'restart' };

export interface PhoneFlowSettings {
  incomingRoute?: "voice" | "windows";
  incomingInitialPrompt?: string;
  voiceDefault?: PhoneVoiceSelection;
  outgoingRoute?: "voice" | "windows";
  accessCodePath?: string | null;
  speech?: PhoneSpeechSettings;
  windowsAudio?: Record<string, unknown>;
  application: { appUserModelId: string; startIfMissing: boolean };
  audio: Record<string, unknown>;
  voiceInput: Record<string, unknown>;
  ringSeconds: number;
  voiceArchive: PhoneVoiceArchiveSettings | null;
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
      "The original Phone command did not return a confirmed result. Read its retained outcome.",
      "unknown",
    );
  return operation.receipt.result as Record<string, unknown>;
}

/** Call-domain sequencing over the existing journal. Promises are only current execution, not a
 * second durable result store. Every effect goes through the original PhoneCallCommands ledger. */
export class PhoneFlow {
  private readonly work = new Map<string, Promise<void>>();
  private readonly admissions = new Set<Promise<PhoneCall>>();
  private readonly commandSequences = new Map<string, number>();
  private readonly endedCalls = new Set<string>();
  private readonly abandonedCalls = new Set<string>();
  private readonly audioRebinds = new Set<string>();
  private readonly voiceSelections = new Map<string, PhoneVoiceSelection>();
  private closed = false;
  private closing: Promise<void> | null = null;
  private readonly settings: PhoneFlowSettings;
  private readonly appToolsPool = new PhoneAppToolsPool();
  private readonly controller: PhoneVoiceController;
  private readonly archives: Pick<PhoneVoiceArchive, "run" | "reconcile">;
  private readonly voiceTasks: Pick<
    PhoneVoiceTasks,
    "prepare" | "bind" | "prompt" | "select" | "release"
  >;
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
    archives?: Pick<PhoneVoiceArchive, "run" | "reconcile">,
    readonly logs?: PhoneCallLogs,
    voiceTasks?: Pick<
      PhoneVoiceTasks,
      "prepare" | "bind" | "prompt" | "select" | "release"
    >,
    private readonly onConnected: (callId: string) => void = () => undefined,
    private readonly audioOwnerProof?: (call: PhoneCall, target: PhoneCallTarget, threadId: string) => Promise<void>,
  ) {
    validatePhone("CallDesktopLaunch", {
      callId: "00000000-0000-4000-8000-000000000001",
      application: settings.application,
    });
    validatePhone("AudioSettings", settings.audio);
    validatePhone("VoiceInputSettings", settings.voiceInput);
    requireThat(
      Number.isInteger(settings.ringSeconds) &&
        settings.ringSeconds >= 1 &&
        settings.ringSeconds <= 120,
      "invalid_arguments",
      "Phone ringing must be bounded.",
    );
    this.settings = structuredClone(settings);
    const openAppTools = (settings: NonNullable<PhoneFlowSettings["voiceArchive"]>["appTools"]) =>
      this.appToolsPool.open(settings);
    this.controller = new PhoneVoiceController(openAppTools);
    this.voiceTasks =
      voiceTasks ??
      new PhoneVoiceTasks(calls.journal, openAppTools, this.controller);
    this.archives =
      archives ??
      new PhoneVoiceArchive(calls.journal, openAppTools, this.controller);
  }
  async prewarmAppTools(): Promise<void> {
    const archive = this.settings.voiceArchive;
    if (!archive) return;
    const resolved = await this.controller.resolve(archive.databasePath, archive.appTools);
    await this.appToolsPool.prewarm(resolved);
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
      requireThat(forwarded.intent.method === 'call.forwardVoice' && forwarded.intent.callId,
        'mutation_conflict', 'Phone request operation belongs to another command.');
      const admission = this.calls.admission.outgoing(forwarded.intent.epoch, operationId, principalId,
        recipientId, selectedRoute, undefined, destination, voicePrompt);
      requireThat(forwarded.intent.requestHash === digest(canonical(admission)),
        'mutation_conflict', 'Original forwarded Phone request changed.');
      const call = this.original(principalId, forwarded.intent.callId, 'read');
      return this.admit(() => this.forwardVoiceRequest(call, admission));
    }
    if (selectedRoute === 'voice' && !this.calls.journal.callForOperation(operationId)) {
      requireThat(this.settings.voiceArchive &&
        (this.settings.voiceInput['micro'] || this.settings.voiceInput['hotkeyFallback']),
        'phone_voice_unconfigured', 'Voice calls require configured task control and Voice input before dialing.');
      if (voicePrompt && recipientId) {
        const call = this.calls.journal.currentCalls().find(candidate => this.callRoute(candidate) === 'voice' &&
          this.calls.admission.matchesRecipient(candidate, recipientId));
        if (call) {
          const admission = this.calls.admission.outgoing(this.native.epoch, operationId, principalId,
            recipientId, selectedRoute, undefined, destination, voicePrompt);
          return this.admit(() => this.forwardVoiceRequest(call, admission));
        }
      }
    }
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
  private async forwardVoiceRequest(call: PhoneCall, admission: PhoneCallAdmission): Promise<PhoneCall> {
    // Setup, task switches and other prompts share the same per-call execution slot.
    while (this.work.has(call.callId)) await this.work.get(call.callId)!.catch(() => undefined);
    const requestHash = digest(canonical(admission));
    const prior = this.calls.journal.get(admission.operationId);
    if (prior) {
      requireThat(prior.intent.method === 'call.forwardVoice' && prior.intent.callId === call.callId &&
        prior.intent.requestHash === requestHash,
        'mutation_conflict', 'Original forwarded Phone request changed.');
      const state = (prior.receipt?.result as { state?: unknown } | null)?.state;
      if (prior.phase === 'result' && prior.receipt?.ok && state === 'sent') return call;
      throw new IvyError('phone_voice_prompt_unknown',
        'Original forwarded Voice prompt remains unknown and cannot be repeated.', 'unknown');
    }
    this.continuing(call);
    const pending = Promise.resolve().then(async () => {
      this.continuing(call);
      const target = this.calls.journal.target(call.callId);
      const generation = this.calls.journal.latestVoiceGeneration(call.callId);
      requireThat(target?.voiceArchive && generation !== null,
        'phone_voice_task_missing', 'The current call has no bound Voice task.');
      const threadId = this.calls.journal.voiceTask(call.callId, generation);
      requireThat(threadId, 'phone_voice_task_missing', 'The current Voice task is unavailable.');
      await this.voiceTasks.prompt(call, target.voiceArchive.databasePath, target.voiceArchive.appTools,
        generation, threadId, admission.voicePrompt!, async () => {
          await this.confirmConnectedForVoice(call);
        }, this.voiceSelection(call.callId), { operationId: admission.operationId, requestHash });
    }).finally(() => this.work.delete(call.callId));
    this.work.set(call.callId, pending);
    await pending;
    return call;
  }
  screen(
    principalId: string,
    operationId: string,
    recipientId: string,
    screening: PhoneScreening,
  ): Promise<PhoneCall> {
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
  private windowsAudio(): Record<string, unknown> {
    return this.settings.windowsAudio
      ? { ...this.settings.windowsAudio }
      : { ...this.settings.audio, sourceMode: "system_excluding_runtime" };
  }
  audioProfiles() {
    return structuredClone([
      { route: "voice", settings: this.settings.audio },
      { route: "windows", settings: this.windowsAudio() },
    ]);
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
    };
  }
  voiceSelection(callId: string): PhoneVoiceSelection {
    return this.voiceSelections.get(callId) ?? this.settings.voiceDefault ?? defaultPhoneVoiceSelection;
  }
  callRoute(call: PhoneCall): 'voice' | 'windows' {
    return call.route ?? (call.direction === 'incoming'
      ? this.settings.incomingRoute ?? 'voice'
      : this.settings.outgoingRoute ?? 'windows');
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
  private async cleanupWhenObserved(call: PhoneCall, observed?: PhoneStatus): Promise<void> {
    if (!this.calls.journal.currentCall(call.callId)) {
      this.abandonedCalls.delete(call.callId);
      return;
    }
    const status = observed ?? await this.native.observe(call.callId);
    if (status.call?.id !== call.callId) {
      const created = this.calls.journal.callCommand(call.callId,
        call.direction === 'incoming' ? 'call.claim' : 'call.prepare');
      if (!created || created.phase === 'result' && created.receipt?.ok === false) {
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
  private async target(call: PhoneCall): Promise<PhoneCallTarget> {
    const retained = this.calls.journal.target(call.callId);
    if (retained) return retained;
    this.continuing(call);
    const launch = result(
      await this.calls.launchDesktop(
        call.principalId,
        call.callId,
        this.settings.application,
      ),
    );
    const launched = launch["result"] as {
      phase: string;
      identity: Record<string, unknown> | null;
    };
    requireThat(
      ["ready", "waiting", "submitted", "not_submitted", "outcome_unknown"].includes(launched.phase),
      "phone_desktop_unavailable",
      "Desktop activation did not confirm its original owner.",
    );
    const retain = (desktop: Record<string, unknown>) => {
      this.continuing(call);
      requireThat(
        desktop["appUserModelId"] === this.settings.application.appUserModelId,
        "phone_desktop_unavailable",
        "Desktop belongs to another application.",
      );
      return this.calls.journal.retainTarget({
        callId: call.callId,
        desktop,
        audio: this.settings.audio,
        voiceInput: this.settings.voiceInput,
        voiceArchive: this.settings.voiceArchive,
      });
    };
    if (launched.phase === "ready") {
      requireThat(
        launched.identity,
        "phone_desktop_unavailable",
        "Ready Desktop requires its original identity.",
      );
      return retain(launched.identity);
    }
    // A refused activation can race with another Desktop startup. Observe only;
    // never repeat the original activation effect.
    const began = performance.now();
    while (performance.now() - began < 10000) {
      this.continuing(call);
      const observed = await this.native.observeDesktop(
        this.settings.application.appUserModelId,
      );
      if (observed.state === "ready" && observed.identity) {
        return retain(observed.identity);
      }
      requireThat(
        observed.state === "absent" || observed.state === "waiting",
        "phone_desktop_unavailable",
        "Desktop ownership is ambiguous or unavailable.",
      );
      await delay(100);
    }
    throw new IvyError(
      "phone_desktop_unavailable",
      "Original Desktop activation did not become ready before its deadline.",
    );
  }
  private async prepareVoice(call: PhoneCall): Promise<PhoneCallTarget> {
    const selected = await this.target(call);
    await this.prepareVoiceAudio(call, selected);
    return selected;
  }
  private async prepareVoiceAudio(call: PhoneCall, selected: PhoneCallTarget): Promise<void> {
    this.continuing(call);
    const audio = result(
      await this.calls.prepareAudio(
        call.principalId,
        call.callId,
        selected.audio,
        selected.desktop,
      ),
    );
    validatePhone("AudioAttachmentStatus", audio);
    requireThat(
      audio["callId"] === call.callId && audio["state"] === "suspended",
      "phone_audio_not_ready",
      "Original audio preparation must begin suspended.",
    );
    this.continuing(call);
  }
  private async startVoice(
    call: PhoneCall,
    selected: PhoneCallTarget,
    generation = 0,
    preparedThreadId?: string | null,
  ): Promise<string | null> {
    requireThat(
      selected.voiceArchive,
      "phone_app_tools_unconfigured",
      "Voice requires direct Codex App Tools task control.",
    );
    if (preparedThreadId === undefined)
      preparedThreadId = await this.prepareVoiceTask(call, selected, generation);
    try {
      this.continuing(call);
      const voice = result(
        await this.calls.startVoice(
          call.principalId,
          call.callId,
          selected.desktop,
          selected.voiceInput,
        ),
      );
      if (
        (voice["result"] as { state?: string } | undefined)?.state !== "active"
      )
        throw new IvyError(
          "phone_voice_not_ready",
          "The original Voice session is not confirmed active. Its retained result controls cleanup.",
          "unknown",
        );
      this.continuing(call);
      return preparedThreadId;
    } catch (error) {
      await this.voiceTasks.release(call.callId, generation);
      throw error;
    }
  }
  private async prepareVoiceTask(
    call: PhoneCall,
    selected: PhoneCallTarget,
    generation = 0,
  ): Promise<string | null> {
    requireThat(
      selected.voiceArchive,
      "phone_app_tools_unconfigured",
      "Voice requires direct Codex App Tools task control.",
    );
    return this.voiceTasks.prepare(
      call,
      selected.voiceArchive.databasePath,
      selected.voiceArchive.appTools,
      generation,
      this.voiceSelection(call.callId),
    );
  }
  private async bindVoice(
    call: PhoneCall,
    selected: PhoneCallTarget,
    generation: number,
    preparedThreadId: string | null,
  ): Promise<string> {
    requireThat(
      selected.voiceArchive,
      "phone_app_tools_unconfigured",
      "Voice requires direct Codex App Tools task control.",
    );
    requireThat(generation === 0 || preparedThreadId, 'phone_voice_task_missing', 'Voice restart requires the prepared Codex task.');
    try {
      const threadId = await this.voiceTasks.bind(
        call,
        selected.voiceArchive.databasePath,
        selected.voiceArchive.appTools,
        generation,
        preparedThreadId,
        true,
      );
      requireThat(threadId, 'phone_voice_task_missing', 'The initial Voice prompt needs a bound Codex task.');
      return threadId;
    } catch (error) {
      try {
        this.onError(call.callId, IvyError.from(error).code);
      } catch {}
      throw error;
    }
  }
  private async promptVoice(
    call: PhoneCall,
    selected: PhoneCallTarget,
    generation: number,
    threadId: string,
  ): Promise<boolean> {
    requireThat(
      selected.voiceArchive,
      "phone_app_tools_unconfigured",
      "Voice prompt requires direct Codex App Tools task control.",
    );
    try {
      await this.voiceTasks.prompt(
        call,
        selected.voiceArchive.databasePath,
        selected.voiceArchive.appTools,
        generation,
        threadId,
        this.initialVoicePrompt(call),
        async () => {
          await this.confirmConnectedForVoice(call);
        },
        this.voiceSelection(call.callId),
      );
      return true;
    } catch (error) {
      // The SIP and Voice session already exist. The original prompt outcome is
      // journaled and cannot be submitted again; leave the conversation available.
      if (IvyError.from(error).code === 'phone_call_cancelled') throw error;
      try { this.onError(call.callId, IvyError.from(error).code); } catch {}
      return false;
    }
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
        if (IvyError.from(error).code !== 'invalid_arguments') throw error;
        if (attempt === 2)
          throw new IvyError('phone_voice_status_invalid',
            'Native call status remained invalid after SIP answer.');
        await delay(100 * (attempt + 1));
        continue;
      }
      this.continuing(call);
      requireThat(observed.call?.id === call.callId && observed.call.state === 'connected',
        'phone_call_cancelled', 'Original call ended before Voice submission.');
      return;
    }
  }
  private initialVoicePrompt(call: PhoneCall): string {
    const greeting = call.direction === 'incoming'
      ? (this.settings.incomingInitialPrompt ?? defaultIncomingInitialPrompt)
      : 'Begrüße die angerufene Person zuerst kurz auf Deutsch.';
    return call.voicePrompt ? `${greeting}\n\n${call.voicePrompt}` : greeting;
  }
  private async connect(call: PhoneCall): Promise<void> {
    this.continuing(call);
    // The single current call needs enough history for its full normal path and cleanup before
    // it opens Voice. Actual writes still reserve atomically in the existing journal.
    const methods: PhoneJournalIntent["method"][] = [
      "call.desktop.launch",
      "call.audio.prepare",
      "call.audio.rebind",
      "call.desktop.startVoice",
      "call.createVoice",
      "call.bindVoice",
      "call.features",
      "call.windows.connect",
      "call.codec.upgrade",
      "call.screening.synthesize",
      "call.screening.prepare",
      "call.screening.bridge",
      "call.promptVoice",
      "call.waiting.end",
      "call.hangup",
      "call.desktop.stopVoice",
      "call.release",
      ...(call.direction === "outgoing"
        ? (["call.prepare", "call.dial"] as const)
        : (["call.claim", "call.answer"] as const)),
    ];
    const remaining =
      methods.filter(
        (method) => !this.calls.journal.callCommand(call.callId, method),
      ).length +
      (this.settings.voiceArchive &&
      !this.calls.journal.callCommand(call.callId, "call.archiveVoice")
        ? 1
        : 0);
    const usage = this.calls.journal.status(),
      target = this.calls.journal.target(call.callId);
    requireThat(
      usage.operations + remaining <= this.calls.journal.limits.maxOperations &&
        usage.reservedBytes +
          (remaining + (target ? 0 : 1)) * phoneOutcomeReservation <=
          this.calls.journal.limits.maxBytes,
      "phone_journal_capacity",
      "Phone requires room for the complete original call and cleanup before Voice effects.",
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
        "Incoming challenge requires its protected code settings.",
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
        "Original screening requires speech synthesis.",
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
    const route = this.callRoute(call);
    const earlyAnswer = call.direction === "incoming" && !call.screening && !challenged && route === "voice";
    if (earlyAnswer) {
      this.continuing(call);
      this.callResult(call, await this.calls.answer(call.principalId, call.callId, true), ["connected"]);
      try { this.onConnected(call.callId); } catch { /* Diagnostics cannot change a connected call. */ }
    }
    // Prepare the selected Desktop and suspended audio before dialing. App Tools may be
    // prewarmed concurrently with outgoing ringing, but Codex Voice itself never starts
    // until SIP confirms that the peer answered. Otherwise Realtime can speak before the
    // remote media path exists and swallow the beginning of the conversation. Incoming
    // Voice calls are already answered with local progress audio at this point.
    let selected: PhoneCallTarget | null = null,
      voiceStarted = false,
      preparedThreadId: string | null = null,
      threadId: string | null = null;
    if (!call.screening && !challenged && route === "voice") {
      if (call.direction === "incoming") {
        selected = await this.target(call);
        const [audio, task] = await Promise.allSettled([
          this.prepareVoiceAudio(call, selected),
          this.prepareVoiceTask(call, selected),
        ]);
        if (audio.status === 'rejected') {
          await this.voiceTasks.release(call.callId, 0);
          throw audio.reason;
        }
        if (task.status === 'rejected') {
          await this.voiceTasks.release(call.callId, 0);
          throw task.reason;
        }
        preparedThreadId = task.value;
        preparedThreadId = await this.startVoice(call, selected, 0, preparedThreadId);
        voiceStarted = true;
        threadId = await this.bindVoice(call, selected, 0, preparedThreadId);
      } else {
        selected = await this.prepareVoice(call);
      }
    }
    const credentials =
      call.direction === "outgoing" ? await this.credentials() : null;
    this.continuing(call);
    if (call.direction === "outgoing") {
      const dialing = this.calls.dial(
          call.principalId,
          call.callId,
          credentials!,
          this.settings.ringSeconds,
          selected !== null,
        ),
        prewarming = selected
          ? this.prepareVoiceTask(call, selected)
          : Promise.resolve(null);
      // Observe both rejections immediately, including a fast App Tools failure
      // while the SIP peer is still ringing. Cleanup waits for both owners.
      const [dialed, prewarmed] = await Promise.allSettled([dialing, prewarming]);
      if (dialed.status === 'rejected') throw dialed.reason;
      this.callResult(call, dialed.value, ["connected"]);
      if (prewarmed.status === 'rejected') throw prewarmed.reason;
      preparedThreadId = prewarmed.value;
    } else if (!earlyAnswer) {
      this.callResult(
        call,
        await this.calls.answer(call.principalId, call.callId),
        ["connected"],
      );
    }
    if (!earlyAnswer) try {
      this.onConnected(call.callId);
    } catch { /* Diagnostics cannot change a connected call. */ }
    if (call.screening) return;
    if (call.direction === "outgoing" && selected) {
      preparedThreadId = await this.startVoice(call, selected, 0, preparedThreadId);
      voiceStarted = true;
    }
    if (voiceStarted && selected) {
      if (!threadId) threadId = await this.bindVoice(call, selected, 0, preparedThreadId);
      requireThat(threadId && selected.voiceArchive, 'phone_voice_task_missing', 'The initial Voice prompt needs a bound Codex task.');
      if (earlyAnswer || call.direction === 'outgoing') {
        this.continuing(call);
        result(await this.calls.endWaiting(call.principalId, call.callId));
      }
      await this.promptVoice(call, selected, 0, threadId);
    }
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
    if (!selected) selected = await this.prepareVoice(call);
    if (!voiceStarted) {
      preparedThreadId = await this.startVoice(call, selected);
      threadId = await this.bindVoice(call, selected, 0, preparedThreadId);
      requireThat(threadId && selected.voiceArchive, 'phone_voice_task_missing', 'The initial Voice prompt needs a bound Codex task.');
      await this.promptVoice(call, selected, 0, threadId);
    }
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
  /** Periodic observation handles remote hangup; it never resumes or redials accepted work. */
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
        } catch {
          /* Optional diagnostics never prevent cleanup. */
        }
        const ended =
          status.call?.id === call.callId &&
          (["local_ended", "outcome_unknown"].includes(status.call.state) ||
            status.media?.audio?.state === "revoked" || status.media?.failed === true);
        if (ended) this.endedCalls.add(call.callId);
        if (this.work.has(call.callId)) continue;
        if (status.call?.id === call.callId && status.call.state === 'connected' &&
            status.media?.audio?.state === 'failed' && !ended) {
          if (!this.audioRebinds.has(call.callId)) this.scheduleAudioRebind(call);
          else await this.cleanupWhenObserved(call, status);
          continue;
        }
        if (ended || this.abandonedCalls.has(call.callId))
          await this.cleanupWhenObserved(call, status);
        else if (status.call?.id === call.callId && status.call.state === "connected") {
          const previous = this.commandSequences.get(call.callId) ?? 0;
          const next = status.call.features?.commands.find(item => item.sequence > previous);
          if (!next) continue;
          if (next.sequence !== previous + 1)
            try { this.onError(call.callId, 'phone_command_overflow'); } catch {}
          this.commandSequences.set(call.callId, next.sequence);
          if (next.command === "new_voice") this.scheduleRecycle(call, next.sequence);
          else {
            requireThat(next.model && next.reasoningEffort,
              'phone_voice_selection_invalid', 'Native Voice selection has no model or reasoning.');
            this.scheduleSelection(call, next.sequence,
              selectedPhoneVoiceModel(next.model, next.reasoningEffort));
          }
        }
      } catch (error) {
        try {
          this.onError(call.callId, IvyError.from(error).code);
        } catch {}
      }
    }
  }
  private scheduleAudioRebind(call: PhoneCall): void {
    this.audioRebinds.add(call.callId);
    const pending = Promise.resolve()
      .then(() => this.rebindAudio(call))
      .catch(async error => {
        try { this.onError(call.callId, IvyError.from(error).code); } catch {}
        this.abandonedCalls.add(call.callId);
        try { await this.cleanupWhenObserved(call); }
        catch (cleanupError) {
          try { this.onError(call.callId, IvyError.from(cleanupError).code); } catch {}
        }
      })
      .finally(() => this.work.delete(call.callId));
    this.work.set(call.callId, pending);
  }
  private async rebindAudio(call: PhoneCall): Promise<void> {
    this.continuing(call);
    const target = this.calls.journal.target(call.callId);
    const generation = this.calls.journal.latestVoiceGeneration(call.callId);
    const threadId = generation === null ? null : this.calls.journal.voiceTask(call.callId, generation);
    requireThat(target?.voiceArchive && threadId,
      'phone_audio_owner_changed', 'Audio recovery requires the original bound Voice task.');
    if (this.audioOwnerProof) await this.audioOwnerProof(call, target, threadId);
    else await this.confirmAudioOwner(call, target, threadId);
    this.continuing(call);
    const repaired = result(await this.calls.rebindAudio(call.principalId, call.callId, target.desktop, threadId));
    validatePhone('AudioAttachmentStatus', repaired);
    requireThat(repaired['callId'] === call.callId && repaired['state'] === 'open',
      'phone_audio_not_ready', 'The original call audio did not reopen.');
  }
  private async confirmAudioOwner(call: PhoneCall, target: PhoneCallTarget, threadId: string): Promise<void> {
    requireThat(target.voiceArchive, 'phone_audio_owner_changed', 'Bound Voice archive is unavailable.');
    const original = target.desktop as NonNullable<PhoneDesktopObservation['identity']>;
    const desktop = await this.native.observeDesktop(original.appUserModelId);
    this.continuing(call);
    requireThat(desktop.state === 'ready' && desktop.identity?.pid === original.pid &&
      desktop.identity.startTimeUtcTicks === original.startTimeUtcTicks &&
      desktop.identity.appUserModelId === original.appUserModelId &&
      desktop.identity.imagePath.toLowerCase() === original.imagePath.toLowerCase(),
      'phone_audio_owner_changed', 'The original Codex Desktop is no longer current.');
    requireThat(readDesktopVoiceReference(target.voiceArchive.databasePath) === threadId,
      'phone_audio_owner_changed', 'Desktop Voice no longer names the bound task.');
    const resolved = await this.controller.resolve(target.voiceArchive.databasePath, target.voiceArchive.appTools);
    const observed = await this.appToolsPool.open(resolved).readThread(threadId);
    this.continuing(call);
    requireThat(observed.id === threadId && ['active', 'idle'].includes(observed.status.type) &&
      readDesktopVoiceReference(target.voiceArchive.databasePath) === threadId,
      'phone_audio_owner_changed', 'The original local Voice task is no longer current.');
  }
  private scheduleSelection(call: PhoneCall, commandSequence: number, selection: PhoneVoiceSelection): void {
    if (this.work.has(call.callId)) return;
    const pending = this.applySelection(call, { commandSequence }, selection)
      .then(() => this.commandFeedback(call, commandSequence, true), async error => {
        try { this.onError(call.callId, IvyError.from(error).code); } catch {}
        await this.commandFeedback(call, commandSequence, false);
      })
      .finally(() => this.work.delete(call.callId));
    this.work.set(call.callId, pending);
  }
  private async commandFeedback(call: PhoneCall, commandSequence: number, success: boolean): Promise<void> {
    try {
      this.continuing(call);
      result(await this.calls.feedback(call.principalId, call.callId, commandSequence, success));
    } catch (error) {
      try { this.onError(call.callId, IvyError.from(error).code); } catch {}
    }
  }
  async controlVoice(principalId: string, callId: string, operationId: string, control: PhoneVoiceControl): Promise<PhoneOperation> {
    const call = this.original(principalId, callId, 'read');
    const prior = this.calls.journal.get(operationId);
    if (prior) {
      requireThat(prior.intent.callId === callId && (control.action === 'select'
        ? prior.intent.method === 'call.selectVoice' && prior.intent.model === control.selection.model && prior.intent.reasoningEffort === control.selection.reasoningEffort
        : prior.intent.method === 'call.restartVoice'),
      'mutation_conflict', 'Voice operation belongs to another command or selection.');
      return prior;
    }
    this.continuing(call);
    requireThat(this.callRoute(call) === 'voice', 'phone_voice_task_missing', 'Voice controls require a Voice call.');
    requireThat(!this.work.has(callId), 'phone_call_busy', 'Wait for the current call action before changing Voice.');
    // Reserve the same per-call slot used by setup and keypad commands before any await.
    const pending = Promise.resolve().then(() => control.action === 'select'
      ? this.applySelection(call, { operationId }, control.selection)
      : this.recycle(call, operationId)).finally(() => this.work.delete(callId));
    this.work.set(callId, pending);
    await pending;
    const operation = this.calls.journal.get(operationId);
    requireThat(operation, 'phone_operation_missing', 'Voice command has no retained outcome.');
    return operation;
  }
  private async applySelection(call: PhoneCall, command: PhoneVoiceSelectionCommand, selection: PhoneVoiceSelection): Promise<void> {
    this.continuing(call);
    const target = this.calls.journal.target(call.callId);
    const generation = this.calls.journal.latestVoiceGeneration(call.callId);
    requireThat(target?.voiceArchive && generation !== null,
      'phone_voice_task_missing', 'Current Voice task is not bound for model selection.');
    const threadId = this.calls.journal.voiceTask(call.callId, generation);
    requireThat(threadId, 'phone_voice_task_missing', 'Current Voice task is unavailable.');
    await this.voiceTasks.select(call, target.voiceArchive.databasePath, target.voiceArchive.appTools,
      threadId, command, selection, async () => {
        await this.confirmConnectedForVoice(call);
      });
    this.continuing(call);
    this.voiceSelections.set(call.callId, selection);
  }
  private scheduleRecycle(call: PhoneCall, commandSequence: number): void {
    if (this.work.has(call.callId)) return;
    const pending = this.recycle(call)
      .then(() => this.commandFeedback(call, commandSequence, true), async (error) => {
        try {
          this.onError(call.callId, IvyError.from(error).code);
        } catch {}
        await this.commandFeedback(call, commandSequence, false);
      })
      .finally(() => this.work.delete(call.callId));
    this.work.set(call.callId, pending);
  }
  private async recycle(call: PhoneCall, operationId?: string): Promise<void> {
    this.continuing(call);
    const target = this.calls.journal.target(call.callId),
      generation = this.calls.journal.latestVoiceGeneration(call.callId);
    requireThat(
      target?.voiceArchive && generation !== null,
      "phone_voice_task_missing",
      "Current Voice generation is not bound to an App task.",
    );
    const threadId = this.calls.journal.voiceTask(call.callId, generation);
    requireThat(
      threadId && generation < 128,
      "phone_voice_generation_exhausted",
      "Current Voice generation cannot be recycled.",
    );
    // Create, bind, prompt and command outcome/feedback, plus each call's cleanup.
    const remaining = 4 + this.calls.journal.currentCalls().length * 4,
      usage = this.calls.journal.status();
    requireThat(
      usage.operations + remaining <= this.calls.journal.limits.maxOperations &&
      usage.reservedBytes + remaining * phoneOutcomeReservation <=
          this.calls.journal.limits.maxBytes,
      "phone_journal_capacity",
      "Phone requires room for the new Voice task and its cleanup before transfer.",
    );
    // Keep the current Voice session live while its new task is prepared.
    // App Tools transfers the session to that task during binding.
    await this.confirmConnectedForVoice(call);
    const next = generation + 1;
    const selection = this.voiceSelection(call.callId);
    const intent: PhoneJournalIntent & { method: 'call.restartVoice' } | null = operationId ? {
      epoch: call.epoch, callId: call.callId, operationId, method: 'call.restartVoice',
      voiceGeneration: next, ...selection,
      requestHash: digest(canonical({ voiceGeneration: next, ...selection })),
    } : null;
    if (intent) this.calls.journal.submit(intent);
    try {
      const preparedThreadId = await this.voiceTasks.prepare(
        call, target.voiceArchive.databasePath, target.voiceArchive.appTools, next, selection,
      );
      this.continuing(call);
      const nextThreadId = await this.voiceTasks.bind(
        call, target.voiceArchive.databasePath, target.voiceArchive.appTools, next, preparedThreadId, true,
      );
      const prompted = await this.promptVoice(call, target, next, nextThreadId);
      if (intent) this.calls.journal.finishVoiceRestart(intent, prompted ? nextThreadId : null);
    } catch (error) {
      if (intent && this.calls.journal.epoch === call.epoch && this.calls.journal.get(intent.operationId)?.phase === 'submitted')
        this.calls.journal.finishVoiceRestart(intent, null);
      throw error;
    }
  }
  async hangup(principalId: string, callId: string): Promise<void> {
    this.original(principalId, callId, "cleanup");
    await this.cleanup(principalId, callId);
  }
  async reconcileArchive(
    principalId: string,
    callId: string,
    generation = 0,
  ): Promise<PhoneOperation | null> {
    this.original(principalId, callId, "cleanup");
    await this.archives.reconcile(callId, generation);
    return this.calls.journal.callCommand(
      callId,
      "call.archiveVoice",
      generation,
    );
  }
  private async cleanup(principalId: string, callId: string): Promise<void> {
    const call = this.original(principalId, callId, "cleanup");
    if (call.epoch === this.native.epoch && this.calls.journal.currentCall(callId))
      this.endedCalls.add(callId);
    await this.voiceTasks.release(callId);
    if (
      call.epoch !== this.native.epoch ||
      this.calls.journal.currentCall(callId)?.callId !== callId
    )
      return;
    if (this.logs) {
      try {
        this.logs.record(call, await this.native.observe(callId));
      } catch {
        /* Diagnostics never prevent original cleanup. */
      }
    }
    if (call.direction === "incoming") {
      const claim = this.calls.journal.callCommand(callId, "call.claim");
      if (
        !claim ||
        (claim.phase === "result" &&
          claim.receipt?.ok === false &&
          claim.receipt.error === "phone_offer_expired")
      ) {
        this.calls.cancelUnprepared(principalId, callId);
        this.endedCalls.delete(callId);
        this.abandonedCalls.delete(callId);
        return;
      }
    }
    if (
      call.direction === "outgoing" &&
      !this.calls.journal.callCommand(callId, "call.prepare")
    ) {
      this.calls.cancelUnprepared(principalId, callId);
      this.endedCalls.delete(callId);
      this.abandonedCalls.delete(callId);
      return;
    }
    // An unknown SIP outcome still permits original Voice cleanup and native resource release.
    // It remains unknown in its receipt even after local resources have been released.
    this.callResult(call, await this.calls.hangup(principalId, callId), [
      "local_ended",
      "outcome_unknown",
    ]);
    const target = this.calls.journal.target(callId);
    if (
      target &&
      this.calls.journal.callCommand(callId, "call.desktop.startVoice")
    ) {
      const stopped = result(
        await this.calls.stopVoice(
          principalId,
          callId,
          target.desktop,
          target.voiceInput,
        ),
      );
      if (
        !["stopped", "not_started"].includes(
          String((stopped["result"] as { state?: string } | undefined)?.state),
        )
      )
        await this.confirmVoiceCaptureAbsent(target);
      const generation = this.calls.journal.latestVoiceGeneration(callId);
      if (target.voiceArchive && generation !== null)
        for (let index = 0; index <= generation; index++) {
          const threadId = this.calls.journal.voiceTask(callId, index);
          if (!threadId) continue;
          try {
            await this.archives.run(
              call,
              target.voiceArchive,
              async () => {
                const observed = await this.native.observeDesktopCapture(
                  target.desktop as NonNullable<PhoneDesktopObservation["identity"]>,
                  String(target.audio["captureEndpointId"]),
                );
                requireThat(observed.state === "none" && observed.identity === null,
                  "phone_archive_voice_active",
                  "Desktop Voice capture must remain absent after confirmed Voice cleanup.");
              },
              { generation: index, threadId },
            );
          } catch (error) {
            try { this.onError(callId, IvyError.from(error).code); } catch {}
          }
        }
    }
    result(await this.calls.release(principalId, callId));
    this.commandSequences.delete(callId);
    this.endedCalls.delete(callId);
    this.abandonedCalls.delete(callId);
    this.voiceSelections.delete(callId);
    this.audioRebinds.delete(callId);
  }
  /** An unknown native stop is safe to release only after the native owner proves
   * that this exact Desktop capture endpoint has no owner. Observation is retried
   * briefly; the periodic flow observation retries the whole cleanup later. */
  private async confirmVoiceCaptureAbsent(
    target: PhoneCallTarget,
  ): Promise<void> {
    const deadline = performance.now() + 1000;
    while (true) {
      try {
        const observed = await this.native.observeDesktopCapture(
          target.desktop as NonNullable<PhoneDesktopObservation["identity"]>,
          String(target.audio["captureEndpointId"]),
        );
        if (observed.state === "none" && observed.identity === null) return;
      } catch {
        /* No observation is not proof of a safe release. */
      }
      if (performance.now() >= deadline)
        throw new IvyError(
          "phone_voice_cleanup_unknown",
          "Voice cleanup is still unconfirmed; the call remains fenced until capture is absent.",
          "unknown",
        );
      await delay(100);
    }
  }
  close(): Promise<void> {
    if (this.closing) return this.closing;
    this.closed = true;
    this.closing = (async () => {
      await Promise.allSettled(this.admissions);
      try {
        const results = await Promise.allSettled(
          this.calls.journal
            .currentCalls()
            .map((call) => this.cleanup(call.principalId, call.callId)),
        );
        const failed = results.find((result) => result.status === "rejected");
        if (failed?.status === "rejected") throw failed.reason;
      } finally {
        await Promise.allSettled(this.work.values());
        await this.appToolsPool.close();
      }
    })();
    return this.closing;
  }
}
