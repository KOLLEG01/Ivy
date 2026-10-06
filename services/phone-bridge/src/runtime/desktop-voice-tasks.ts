import { randomUUID } from "node:crypto";
import { setTimeout as delay } from "node:timers/promises";
import { canonical, digest } from "../../../../packages/sdk/src/node.js";
import { IvyError, requireThat } from "../../../../packages/sdk/src/node.js";
import { CodexAppTools } from "../../../../packages/sdk/src/codex-app-tools.js";
import type { AppToolsSettings } from "../../../../packages/sdk/src/codex-app-tools.js";
import type { PhoneCall } from "./admission.js";
import { defaultPhoneVoiceSelection } from "./voice-selection.js";
import type { PhoneVoiceSelection } from "./voice-selection.js";
import {
  readDesktopVoiceRoots,
  readDesktopVoiceReference,
  desktopThreadArchiveState,
  readDesktopVoiceSession,
} from "./desktop-voice-inventory.js";
import { PhoneVoiceController } from "./desktop-voice-controller.js";
import type { PhoneJournal, PhoneJournalIntent } from "./journal.js";

export type DesktopVoiceTaskPort = Pick<
  CodexAppTools,
  | "createLocalThread"
  | "transferVoiceCall"
  | "readThread"
  | "sendMessage"
  | "verifyActor"
  | "close"
>;

export type PhoneVoiceSelectionCommand = { commandSequence: number } | { operationId: string };

/** Reuses this caller's last bound chat. A first call adopts Desktop's chat;
 * only an explicit Voice restart creates a new projectless task. */
export class PhoneVoiceTasks {
  private readonly open: (settings: AppToolsSettings) => DesktopVoiceTaskPort;
  private readonly controller: PhoneVoiceController;
  private readonly prepared = new Map<string, DesktopVoiceTaskPort>();
  private readonly voiceRoots = new Map<string, Set<string>>();
  private readonly startupReferences = new Map<string, string | null>();
  private readonly voiceSessions = new Map<string, string | null>();
  constructor(
    private readonly journal: PhoneJournal,
    open: (settings: AppToolsSettings) => DesktopVoiceTaskPort = (settings) =>
      new CodexAppTools(settings),
    controller?: PhoneVoiceController,
  ) {
    this.open = open;
    this.controller = controller ?? new PhoneVoiceController(open);
  }

  /** Prewarm the caller's last chat, or the first Desktop startup. An explicit
   * restart prepares one exact new target; submitted creation is never retried. */
  async prepare(
    call: PhoneCall,
    databasePath: string,
    settings: AppToolsSettings,
    generation = 0,
    selection: PhoneVoiceSelection = defaultPhoneVoiceSelection,
  ): Promise<string | null> {
    this.owner(call);
    this.generation(generation);
    const key = this.key(call.callId, generation);
    const resolved = await this.controller.resolve(databasePath, settings);
    const port = this.take(call.callId, generation) ?? this.open(resolved);
    let retained = false;
    try {
      await port.verifyActor();
      this.owner(call);
      if (generation === 0) {
        // Preserve the baseline across ringing/pre-start verification. Replacing
        // it later could hide the chat that Micro has just started.
        if (!this.voiceRoots.has(key)) {
          this.startupReferences.set(key, await this.reference(databasePath));
          this.voiceRoots.set(key, new Set(readDesktopVoiceRoots(databasePath)));
        }
      }
      let threadId = generation === 0 ? this.journal.reusableVoiceTask(call.principalId)
        : this.journal.createdVoiceTask(call.callId, generation);
      if (generation === 0 && (!threadId || desktopThreadArchiveState(databasePath, threadId) !== 'unarchived')) {
        this.owner(call);
        this.prepared.set(key, port);
        retained = true;
        return null;
      }
      const createdHere = threadId !== null;
      if (!threadId) {
        const prompt = 'Diese Aufgabe ist für einen Telefonanruf vorbereitet. Warte auf die nächste Nachricht.';
        const intent: PhoneJournalIntent & { method: 'call.createVoice' } = {
          epoch: call.epoch, callId: call.callId, operationId: randomUUID(), method: 'call.createVoice',
          ...(generation ? { voiceGeneration: generation } : {}),
          model: selection.model, reasoningEffort: selection.reasoningEffort,
          requestHash: digest(canonical({ generation, prompt, selection })),
        };
        threadId = await port.createLocalThread(prompt, selection, intent.operationId, async () => {
          this.owner(call);
          this.journal.submit(intent);
        });
        this.journal.finishVoiceCreation(intent, threadId);
      } else if (generation > 0 && createdHere) {
        const original = this.journal.callCommand(call.callId, 'call.createVoice', generation);
        requireThat(original?.intent.method === 'call.createVoice' && original.intent.model === selection.model &&
          original.intent.reasoningEffort === selection.reasoningEffort,
          'mutation_conflict', 'Original Voice task selection changed.');
      }
      this.owner(call);
      if (!this.voiceSessions.has(key))
        this.voiceSessions.set(key, readDesktopVoiceSession(databasePath, threadId)?.sessionId ?? null);
      // Keep the live Voice page open until the explicit transfer is confirmed.
      const deadline = performance.now() + 60000;
      while (true) {
        const observed = await port.readThread(threadId);
        this.owner(call);
        // A returning caller may reach the same task while its previous turn is
        // still running. Voice can attach to that task without resubmitting work.
        if (observed.id === threadId && observed.hostId === 'local' && observed.kind === 'codex' &&
          ['idle', 'active'].includes(observed.status.type) && observed.status.activeFlags.length === 0) break;
        requireThat(performance.now() < deadline, 'phone_voice_task_not_loaded',
          'Desktop did not finish loading the prepared task before Micro startup.');
        await delay(100);
      }
      if (generation > 0) this.voiceRoots.set(key, new Set(readDesktopVoiceRoots(databasePath)));
      this.prepared.set(key, port);
      retained = true;
      return threadId;
    } finally { if (!retained) await port.close().catch(() => undefined); }
  }

  async bind(
    call: PhoneCall,
    databasePath: string,
    settings: AppToolsSettings,
    generation: number,
    preparedThreadId: string | null,
    retainForPrompt = false,
  ): Promise<string> {
    this.owner(call);
    this.generation(generation);
    requireThat(generation === 0 && preparedThreadId === null ||
      typeof preparedThreadId === "string" && preparedThreadId.length > 0,
      "invalid_arguments", "An exact prepared Voice task is required.");
    const prior = this.journal.callCommand(
      call.callId,
      "call.bindVoice",
      generation,
    );
    if (prior) {
      requireThat(
        prior.intent.method === "call.bindVoice" &&
          prior.phase === "result" &&
          prior.receipt?.ok,
        "phone_voice_task_unknown",
        "Original Voice task binding is incomplete.",
      );
      const threadId = this.journal.voiceTask(call.callId, generation);
      requireThat(
        threadId,
        "phone_voice_task_unknown",
        "Original Voice task binding is unavailable.",
      );
      requireThat(
        preparedThreadId === null || preparedThreadId === threadId,
        "mutation_conflict",
        "Prepared Voice task changed after the original generation was bound.",
      );
      if (!retainForPrompt) await this.release(call.callId, generation);
      return threadId;
    }
    if (generation === 0 && preparedThreadId === null)
      return this.bindStarted(call, databasePath, settings, retainForPrompt);
    requireThat(preparedThreadId, "invalid_arguments", "An exact restart task is required.");
    requireThat((generation === 0 ? this.journal.reusableVoiceTask(call.principalId)
      : this.journal.createdVoiceTask(call.callId, generation)) === preparedThreadId,
      "phone_voice_task_changed", "Prepared task is not the caller's PhoneBridge task.");
    const resolved = await this.controller.resolve(databasePath, settings);
    let port: DesktopVoiceTaskPort | null = this.take(call.callId, generation);
    let transferConfirmed = false;
    try {
      // Capture can precede the new root and the persisted reference can still
      // name the reusable chat. Do not mistake that stale value for resumed Voice.
      if (generation === 0) await this.waitStartedThread(call, databasePath);
      const retainedReference = await this.reference(databasePath, true);
      // Micro can create a new Voice task before Desktop flushes its last-task setting.
      // Only the single new Voice root since this call's preparation is eligible.
      const baseline = this.voiceRoots.get(this.key(call.callId, generation));
      const startedRoots = (): string[] => {
        const roots = baseline ? readDesktopVoiceRoots(databasePath)
          .filter(id => !baseline.has(id) && id !== preparedThreadId) : [];
        requireThat(roots.length <= 1, 'phone_voice_reference_changed',
          'Desktop created more than one Voice task during this call startup.');
        return roots;
      };
      let started = startedRoots(), sourceThreadId = started[0] ?? retainedReference;
      if (sourceThreadId !== preparedThreadId) {
        // Micro may resume Desktop's last Voice task even after the new page is loaded.
        // Transfer the session it started, then prove the exact prepared task owns Voice.
        requireThat(sourceThreadId, "phone_voice_reference_changed",
          "Desktop did not identify the Voice task started by Micro.");
        if (!port) {
          port = this.open(resolved);
          await port.verifyActor();
        }
        // Native capture becomes active before the App publishes its realtime session.
        // Only a definite negative acknowledgement permits another transfer attempt.
        const transferDeadline = performance.now() + 10000;
        while (true) {
          this.owner(call);
          // Capture can become active before the new Voice thread is persisted.
          // Reobserve only before a transfer whose outcome is still definitely negative.
          const latest = startedRoots();
          requireThat(!started.length || latest[0] === sourceThreadId,
            'phone_voice_reference_changed', 'Desktop Voice startup identity changed before transfer.');
          if (latest.length) { started = latest; sourceThreadId = latest[0]!; }
          const current = await this.reference(databasePath);
          if (!started.length && current === preparedThreadId) break;
          requireThat(current === sourceThreadId || started.length === 1 && current === retainedReference, "phone_voice_reference_changed",
            "Desktop Voice changed tasks before the prepared transfer.");
          this.owner(call);
          try {
            await port.transferVoiceCall(sourceThreadId, preparedThreadId);
            transferConfirmed = true;
            break;
          } catch (error) {
            if ((!started.length || retainedReference !== preparedThreadId) &&
                await this.reference(databasePath) === preparedThreadId) break;
            if (IvyError.from(error).code === "app_tools_voice_not_ready") {
              if (performance.now() >= transferDeadline) {
                if (generation !== 0) throw error;
                // Every attempt was definitely rejected. The single chat started
                // by Micro still owns Voice; preserve that conversation and call.
                requireThat(await this.startedThread(call, databasePath) === sourceThreadId,
                  'phone_voice_reference_changed', 'Desktop Voice changed chats after rejecting reuse.');
                const key = this.key(call.callId, generation);
                this.voiceSessions.set(key, null);
                this.prepared.set(key, port);
                port = null;
                return await this.bindStarted(call, databasePath, resolved, retainForPrompt);
              }
              await delay(200);
              continue;
            }
            // A transport or malformed App reply might follow a completed transfer.
            // Watch the Desktop reference; never resubmit an uncertain mutation.
            while (performance.now() < transferDeadline) {
              this.owner(call);
              await delay(100);
              const observed = await this.reference(databasePath);
              if (observed === preparedThreadId) break;
              requireThat(observed === sourceThreadId || started.length === 1 && observed === retainedReference || observed === null,
                "phone_voice_reference_changed",
                "Desktop Voice moved to another task during transfer reconciliation.");
            }
            if ((!started.length || retainedReference !== preparedThreadId) &&
                await this.reference(databasePath) === preparedThreadId) break;
            throw new IvyError("phone_voice_transfer_unconfirmed",
              "Desktop did not confirm the prepared Voice task after an uncertain transfer.",
              "unknown", { causeCode: IvyError.from(error).code });
          }
        }
        while (!transferConfirmed && await this.reference(databasePath) !== preparedThreadId) {
          this.owner(call);
          requireThat(performance.now() < transferDeadline,
            "phone_voice_reference_changed", "Desktop did not transfer Voice to the prepared task.");
          await delay(100);
        }
      }
      const deadline = performance.now() + 15000;
      while (true) {
        this.owner(call);
        requireThat(
          transferConfirmed || await this.reference(databasePath) === preparedThreadId,
          "phone_voice_reference_changed",
          "Desktop Voice changed its prepared task during Micro startup.",
        );
        try {
          if (!port) {
            port = this.open(resolved);
            await port.verifyActor();
          }
          const observed = await port.readThread(preparedThreadId);
          requireThat(
            observed.id === preparedThreadId &&
              ["idle", "active"].includes(observed.status.type) &&
              observed.status.activeFlags.length === 0,
            "phone_voice_task_missing",
            "Prepared Desktop Voice task is unavailable after Micro startup.",
          );
        } catch (error) {
          const failed = port;
          port = null;
          if (failed) await failed.close().catch(() => undefined);
          requireThat(
            performance.now() < deadline,
            "phone_voice_task_missing",
            `Prepared Desktop Voice task could not be observed after Micro startup; last App observation failed with ${IvyError.from(error).code}.`,
          );
          await delay(100);
          continue;
        }
        // App observation awaits may span a task switch or call cancellation.
        // Recheck ownership before committing; local journal failures are not observations to retry.
        requireThat(
          transferConfirmed || await this.reference(databasePath) === preparedThreadId,
          "phone_voice_reference_changed",
          "Desktop Voice changed its prepared task during binding.",
        );
        this.owner(call);
        const intent: PhoneJournalIntent & { method: "call.bindVoice" } = {
          epoch: call.epoch,
          callId: call.callId,
          operationId: randomUUID(),
          method: "call.bindVoice",
          ...(generation ? { voiceGeneration: generation } : {}),
          threadId: preparedThreadId,
          requestHash: digest(canonical({ generation, threadId: preparedThreadId })),
        };
        this.journal.submit(intent);
        this.journal.finishVoiceBinding(intent, generation === 0 ? started[0] : undefined);
        this.voiceRoots.delete(this.key(call.callId, generation));
        this.startupReferences.delete(this.key(call.callId, generation));
        if (retainForPrompt) {
          this.prepared.set(this.key(call.callId, generation), port);
          port = null;
        }
        return preparedThreadId;
      }
    } finally {
      if (port) await port.close().catch(() => undefined);
    }
  }

  private async bindStarted(
    call: PhoneCall, databasePath: string, settings: AppToolsSettings, retainForPrompt: boolean,
  ): Promise<string> {
    const key = this.key(call.callId, 0);
    const started = () => this.startedThread(call, databasePath);
    let port = this.take(call.callId, 0);
    const deadline = performance.now() + 15000;
    try {
      const threadId = await this.waitStartedThread(call, databasePath);
      while (true) {
        requireThat(await started() === threadId, 'phone_voice_reference_changed',
          'The original Voice chat changed before binding.');
        try {
          if (!port) {
            port = this.open(await this.controller.resolve(databasePath, settings));
            await port.verifyActor();
          }
          const observed = await port.readThread(threadId);
          requireThat(observed.id === threadId && observed.hostId === 'local' && observed.kind === 'codex' &&
            ['idle', 'active'].includes(observed.status.type) && observed.status.activeFlags.length === 0,
            'phone_voice_task_missing', 'Desktop Voice chat is not available yet.');
        } catch (error) {
          if (port) await port.close().catch(() => undefined);
          port = null;
          this.owner(call);
          requireThat(performance.now() < deadline, 'phone_voice_task_missing',
            `Desktop Voice chat observation failed with ${IvyError.from(error).code}.`);
          await delay(100);
          continue;
        }
        requireThat(await started() === threadId, 'phone_voice_reference_changed',
          'Desktop Voice changed chats during binding.');
        this.owner(call);
        const intent: PhoneJournalIntent & { method: 'call.bindVoice' } = {
          epoch: call.epoch, callId: call.callId, operationId: randomUUID(), method: 'call.bindVoice',
          threadId, requestHash: digest(canonical({ generation: 0, threadId })),
        };
        this.journal.submit(intent);
        this.journal.finishVoiceBinding(intent);
        this.voiceRoots.delete(key);
        this.startupReferences.delete(key);
        if (retainForPrompt) { this.prepared.set(key, port); port = null; }
        return threadId;
      }
    } finally { if (port) await port.close().catch(() => undefined); }
  }

  private async startedThread(call: PhoneCall, databasePath: string): Promise<string | null> {
    const key = this.key(call.callId, 0), baseline = this.voiceRoots.get(key);
    requireThat(baseline && this.startupReferences.has(key), 'phone_voice_task_missing',
      'Initial Voice binding requires its pre-start Desktop baseline.');
    const originalReference = this.startupReferences.get(key);
    this.owner(call);
    const reference = await this.reference(databasePath);
    this.owner(call);
    const roots = readDesktopVoiceRoots(databasePath).filter(id => !baseline.has(id));
    requireThat(roots.length <= 1, 'phone_voice_reference_changed',
      'Desktop created more than one Voice chat during this call startup.');
    requireThat(reference === null || reference === originalReference || reference === roots[0],
      'phone_voice_reference_changed', 'Desktop Voice changed chats during startup.');
    return roots[0] ?? null;
  }

  private async waitStartedThread(call: PhoneCall, databasePath: string): Promise<string> {
    const deadline = performance.now() + 15000;
    let threadId: string | null;
    while (!(threadId = await this.startedThread(call, databasePath))) {
      requireThat(performance.now() < deadline, 'phone_voice_task_missing',
        'Desktop did not publish the Voice chat started by Micro.');
      await delay(100);
    }
    return threadId;
  }

  private async waitVoiceSession(call: PhoneCall, databasePath: string, generation: number, threadId: string): Promise<void> {
    const previous = this.voiceSessions.get(this.key(call.callId, generation)), deadline = performance.now() + 15000;
    while (true) {
      this.owner(call);
      const session = readDesktopVoiceSession(databasePath, threadId);
      if (session?.active && session.sessionId !== previous) return;
      requireThat(performance.now() < deadline, 'phone_voice_not_ready',
        'Desktop did not publish a new active Voice session in the bound chat.');
      await delay(100);
    }
  }

  async prompt(
    call: PhoneCall,
    databasePath: string,
    settings: AppToolsSettings,
    generation: number,
    threadId: string,
    prompt: string,
    beforeSubmit: () => Promise<void>,
    selection: PhoneVoiceSelection = defaultPhoneVoiceSelection,
  ): Promise<void> {
    let port = this.take(call.callId, generation);
    try {
      this.owner(call);
      this.generation(generation);
      requireThat(
        this.journal.voiceTask(call.callId, generation) === threadId,
        "phone_voice_task_changed",
        "Prompt target is not the bound Voice generation.",
      );
      const prior = this.journal.callCommand(
        call.callId,
        "call.promptVoice",
        generation,
      );
      if (prior) {
        requireThat(
          prior.intent.method === "call.promptVoice" &&
            prior.intent.threadId === threadId &&
            prior.intent.prompt === prompt &&
            prior.intent.model === selection.model &&
            prior.intent.reasoningEffort === selection.reasoningEffort,
          "mutation_conflict",
          "Original Voice prompt changed.",
        );
        const state = (prior.receipt?.result as { state?: unknown } | null)
          ?.state;
        if (prior.phase === "result" && state === "sent") return;
        throw new IvyError(
          "phone_voice_prompt_unknown",
          "Original Voice prompt remains unknown and cannot be repeated.",
          "unknown",
        );
      }
      const intent: PhoneJournalIntent & { method: "call.promptVoice" } = {
        epoch: call.epoch,
        callId: call.callId,
        operationId: randomUUID(),
        method: "call.promptVoice",
        ...(generation ? { voiceGeneration: generation } : {}),
        threadId,
        prompt,
        model: selection.model,
        reasoningEffort: selection.reasoningEffort,
        requestHash: digest(canonical({ generation, threadId, prompt,
          model: selection.model, reasoningEffort: selection.reasoningEffort })),
      };
      // A slow/missing realtime session suppresses only the greeting. Binding
      // must not turn this optional readiness observation into a SIP hangup.
      await this.waitVoiceSession(call, databasePath, generation, threadId);
      if (!port) {
        const resolved = await this.controller.resolve(databasePath, settings);
        port = this.open(resolved);
        // A recovered prompt has no retained binding observation.
        const observed = await port.readThread(threadId);
        requireThat(
          observed.id === threadId &&
            ["idle", "active"].includes(observed.status.type) &&
            observed.status.activeFlags.length === 0,
          "phone_voice_task_inactive",
          "Bound Voice task is unavailable for prompt delivery.",
        );
      }
      let submitted = false;
      // bind() just verified the exact task on this retained App Tools connection.
      // Send the greeting as soon as the SIP answer is connected.
      try {
        await port.sendMessage(
          threadId,
          prompt,
          intent.operationId,
          async () => {
            await beforeSubmit();
            this.owner(call);
            this.journal.submit(intent);
            submitted = true;
          },
          selection,
        );
        this.journal.finishVoicePrompt(intent, "sent");
      } catch (error) {
        if (submitted && this.journal.epoch === call.epoch)
          this.journal.finishVoicePrompt(intent, "outcome_unknown");
        throw error;
      }
    } finally {
      if (port) await port.close().catch(() => undefined);
    }
  }

  /** Apply a model choice to this same task. App Tools persists the model/effort override
   * for following turns; the original submission is journaled before the App mutation. */
  async select(
    call: PhoneCall,
    databasePath: string,
    settings: AppToolsSettings,
    threadId: string,
    command: PhoneVoiceSelectionCommand,
    selection: PhoneVoiceSelection,
    beforeSubmit: () => Promise<void>,
  ): Promise<void> {
    this.owner(call);
    const commandSequence = 'commandSequence' in command ? command.commandSequence : undefined;
    requireThat(commandSequence === undefined || Number.isSafeInteger(commandSequence) && commandSequence >= 1 && commandSequence <= 2147483647,
      'invalid_arguments', 'Bounded Voice selection command sequence required.');
    const generation = this.journal.latestVoiceGeneration(call.callId);
    requireThat(generation !== null && this.journal.voiceTask(call.callId, generation) === threadId,
      'phone_voice_task_changed', 'Selection target is not the bound Voice task.');
    const prompt = 'Modell und Reasoning aktualisiert. Antworte nicht.';
    const prior = 'operationId' in command ? this.journal.get(command.operationId)
      : this.journal.voiceSelectionCommand(call.callId, command.commandSequence);
    if (prior) {
      requireThat(prior.intent.callId === call.callId && prior.intent.method === 'call.selectVoice' && prior.intent.threadId === threadId &&
        prior.intent.prompt === prompt && prior.intent.model === selection.model &&
        prior.intent.reasoningEffort === selection.reasoningEffort,
        'mutation_conflict', 'Original Voice selection changed.');
      const state = (prior.receipt?.result as { state?: unknown } | null)?.state;
      if (prior.phase === 'result' && state === 'sent') return;
      throw new IvyError('phone_voice_selection_unknown',
        'Original Voice selection remains unknown and cannot be repeated.', 'unknown');
    }
    const intent: PhoneJournalIntent & { method: 'call.selectVoice' } = {
      epoch: call.epoch, callId: call.callId, operationId: 'operationId' in command ? command.operationId : randomUUID(), method: 'call.selectVoice',
      threadId, prompt, model: selection.model, reasoningEffort: selection.reasoningEffort,
      ...(commandSequence === undefined ? {} : { commandSequence }),
      requestHash: digest(canonical({ threadId, prompt, model: selection.model,
        reasoningEffort: selection.reasoningEffort, ...(commandSequence === undefined ? {} : { commandSequence }) })),
    };
    const resolved = await this.controller.resolve(databasePath, settings);
    const port = this.open(resolved);
    let submitted = false;
    try {
      const observed = await port.readThread(threadId);
      this.owner(call);
      requireThat(observed.id === threadId && ['idle', 'active'].includes(observed.status.type) &&
        observed.status.activeFlags.length === 0,
        'phone_voice_task_inactive', 'Bound Voice task is unavailable for selection.');
      try {
        await port.sendMessage(threadId, prompt, intent.operationId, async () => {
          await beforeSubmit();
          this.owner(call);
          this.journal.submit(intent);
          submitted = true;
        }, selection);
        this.journal.finishVoiceSelection(intent, 'sent');
      } catch (error) {
        if (submitted && this.journal.epoch === call.epoch)
          this.journal.finishVoiceSelection(intent, 'outcome_unknown');
        throw error;
      }
    } finally { await port.close().catch(() => undefined); }
  }

  /** Close retained App Tools processes when a call stops before binding or prompting. */
  async release(callId: string, generation?: number): Promise<void> {
    const keys =
      generation === undefined
        ? [...new Set([...this.prepared.keys(), ...this.voiceRoots.keys(), ...this.voiceSessions.keys()])].filter((key) =>
            key.startsWith(`${callId}:`),
          )
        : [this.key(callId, generation)];
    await Promise.all(
      keys.map(async (key) => {
        const port = this.prepared.get(key);
        this.prepared.delete(key);
        this.voiceRoots.delete(key);
        this.startupReferences.delete(key);
        this.voiceSessions.delete(key);
        if (port) await port.close().catch(() => undefined);
      }),
    );
  }

  private take(
    callId: string,
    generation: number,
  ): DesktopVoiceTaskPort | null {
    const key = this.key(callId, generation),
      port = this.prepared.get(key) ?? null;
    this.prepared.delete(key);
    return port;
  }
  private key(callId: string, generation: number): string {
    return `${callId}:${generation}`;
  }

  private async reference(databasePath: string, requireValue = false): Promise<string | null> {
    let lastError: unknown = null;
    for (let attempt = 0; attempt < 10; attempt++) {
      try {
        const value = readDesktopVoiceReference(databasePath);
        lastError = null;
        if (value !== null || !requireValue) return value;
      } catch (error) {
        lastError = error;
        if (!(error instanceof SyntaxError) &&
            (error as NodeJS.ErrnoException).code !== 'ENOENT' &&
            IvyError.from(error).code !== 'phone_voice_reference_invalid') throw error;
      }
      await delay(100);
    }
    if (lastError) throw lastError;
    return null;
  }

  private owner(call: PhoneCall): void {
    requireThat(
      this.journal.epoch === call.epoch &&
        this.journal.currentCall(call.callId)?.callId === call.callId &&
        !this.journal.callCommand(call.callId, 'call.hangup') &&
        !this.journal.callCommand(call.callId, 'call.release'),
      "phone_voice_owner_changed",
      "Original call must own Voice task control.",
    );
  }
  private generation(value: number): void {
    requireThat(
      Number.isInteger(value) && value >= 0 && value <= 128,
      "invalid_arguments",
      "Bounded Voice generation required.",
    );
  }
}
