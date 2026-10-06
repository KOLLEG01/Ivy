import { randomUUID } from 'node:crypto';
import { isAbsolute } from 'node:path';
import { setTimeout as delay } from 'node:timers/promises';
import { canonical, digest } from '../../../../packages/sdk/src/node.js';
import { IvyError, requireThat } from '../../../../packages/sdk/src/node.js';
import { CodexAppTools } from '../../../../packages/sdk/src/codex-app-tools.js';
import type { AppToolsSettings } from '../../../../packages/sdk/src/codex-app-tools.js';
import type { PhoneCall } from './admission.js';
import type { PhoneJournal, PhoneJournalIntent } from './journal.js';
import { phoneArchivePlanBytes } from './journal.js';
import { readDesktopVoiceCandidates, readDesktopVoiceReference, verifyDesktopArchiveActor } from './desktop-voice-inventory.js';
import type { DesktopVoiceCandidate } from './desktop-voice-inventory.js';
import { PhoneVoiceController } from './desktop-voice-controller.js';

export interface PhoneVoiceArchiveSettings { databasePath: string; appTools: AppToolsSettings }
export interface PhoneVoiceArchivePlan { settings: PhoneVoiceArchiveSettings; candidates: DesktopVoiceCandidate[]; operationIds: string[]; threadId?: string }
export interface PhoneVoiceArchiveScope { generation?: number; threadId?: string }
export interface PhoneVoiceArchiveResult { state: 'archived' | 'outcome_unknown'; archivedIds: string[]; failedThreadId: string | null }
export type DesktopArchivePort = Pick<CodexAppTools, 'readThread' | 'archiveThread' | 'close'>;

/** Sequential cleanup after confirmed Voice end. The existing Phone journal retains
 * the entire bounded plan before the first App mutation. It never replays a partial batch. */
export class PhoneVoiceArchive {
  private readonly pending = new Map<string, { key: string; promise: Promise<void> }>();
  private readonly open: (settings: AppToolsSettings) => DesktopArchivePort;
  private readonly controller: PhoneVoiceController;
  constructor(private readonly journal: PhoneJournal,
    open: (settings: AppToolsSettings) => DesktopArchivePort = settings => new CodexAppTools(settings), controller?: PhoneVoiceController) {
    this.open = open; this.controller = controller ?? new PhoneVoiceController(settings => open(settings));
  }

  run(call: PhoneCall, settings: PhoneVoiceArchiveSettings, assertIdle: () => Promise<void>, scope: PhoneVoiceArchiveScope = {}): Promise<void> {
    // Micro does not identify a Desktop task. Never infer ownership from a global Voice list.
    if (!scope.threadId) return Promise.resolve();
    // The original Desktop Voice chat is the call's conversation history in
    // Recents. Retain it even after an explicit restart changes the overlay ID.
    if ((scope.generation ?? 0) === 0 && this.journal.voiceTask(call.callId, 0) === scope.threadId)
      return assertIdle();
    // App-created Phone tasks remain available in the task list; only the legacy
    // dedicated voice_chat path uses the archive plan below.
    if (this.journal.createdVoiceTask(call.callId, scope.generation ?? 0) === scope.threadId ||
      this.journal.reusableVoiceTask(call.principalId) === scope.threadId) return assertIdle();
    // Codex Desktop keeps this exact ID in its Voice overlay after the session ends and
    // attempts to resume it on the next hotkey. Archiving it leaves a permanent stale
    // overlay reference, so retain the App-owned current task and preload it next time.
    if (readDesktopVoiceReference(settings.databasePath) === scope.threadId) return Promise.resolve();
    const generation = scope.generation ?? 0;
    requireThat(Number.isInteger(generation) && generation >= 0 && generation <= 128, 'invalid_arguments', 'Bounded original Voice generation required.');
    const key = canonical({ generation, threadId: scope.threadId ?? null, settings });
    const current = this.pending.get(call.callId);
    if (current) { requireThat(current.key === key, 'mutation_conflict', 'Pending Voice archival belongs to another original scope.'); return current.promise; }
    const work = this.execute(structuredClone(call), structuredClone(settings), assertIdle, { ...scope, generation })
      .finally(() => this.pending.delete(call.callId));
    this.pending.set(call.callId, { key, promise: work }); return work;
  }
  private owner(call: PhoneCall): void {
    requireThat(this.journal.epoch === call.epoch && this.journal.currentCall(call.callId)?.callId === call.callId,
      'phone_archive_owner_changed', 'Original call must still own cleanup before Desktop archival.');
  }
  private async execute(call: PhoneCall, settings: PhoneVoiceArchiveSettings, assertIdle: () => Promise<void>, scope: PhoneVoiceArchiveScope): Promise<void> {
    this.owner(call);
    const prior = this.journal.callCommand(call.callId, 'call.archiveVoice', scope.generation);
    if (prior) {
      requireThat(prior.intent.method === 'call.archiveVoice' && prior.intent.archivePlan.threadId === scope.threadId,
        'mutation_conflict', 'Archive cleanup cannot change its original Voice scope.');
      if (prior.archiveResolution?.state === 'observed_archived' ||
        prior.phase === 'result' && (prior.receipt?.result as PhoneVoiceArchiveResult | null)?.state === 'archived') return;
      throw new IvyError('phone_archive_unknown', 'Original Voice archive remains unresolved; no batch or task is resubmitted.', 'unknown');
    }
    requireThat(isAbsolute(settings.databasePath), 'invalid_arguments', 'Explicit absolute Desktop database required.');
    settings = { databasePath: settings.databasePath, appTools: await this.controller.resolve(settings.databasePath, settings.appTools) };
    verifyDesktopArchiveActor(settings.databasePath, settings.appTools.actorThreadId);
    const inventory = readDesktopVoiceCandidates(settings.databasePath, scope.threadId);
    requireThat(scope.threadId === undefined || inventory.length === 1 && inventory[0]!.threadId === scope.threadId,
      'phone_archive_scope_changed', 'The exact bound Voice task must still exist before archival.');
    // One native root already archives its descendants. Do not submit those descendants again.
    const nested = new Set(inventory.flatMap(candidate => candidate.descendantIds));
    const candidates = inventory.filter(candidate => !nested.has(candidate.threadId));
    requireThat(!inventory.length || candidates.length > 0, 'phone_archive_scope_conflict', 'Voice archive roots are cyclic.');
    const affected = new Set<string>();
    for (const candidate of candidates) for (const id of [candidate.threadId, ...candidate.descendantIds]) {
      requireThat(!affected.has(id) && id !== settings.appTools.actorThreadId, 'phone_archive_scope_conflict', 'Archive subtrees overlap or include their controller.');
      affected.add(id);
    }
    requireThat(affected.size <= 512, 'phone_archive_capacity', 'Combined archive scope exceeds its bounded plan.');
    const plan: PhoneVoiceArchivePlan = { settings, candidates, operationIds: candidates.map(() => randomUUID()), ...(scope.threadId ? { threadId: scope.threadId } : {}) };
    const intent: PhoneJournalIntent & { method: 'call.archiveVoice' } = {
      epoch: call.epoch, callId: call.callId, operationId: randomUUID(), method: 'call.archiveVoice',
      ...(scope.generation ? { voiceGeneration: scope.generation } : {}),
      requestHash: digest(canonical(plan, phoneArchivePlanBytes)), archivePlan: plan,
    };
    const port = this.open(settings.appTools), archivedIds: string[] = [];
    let submitted = false, failedThreadId: string | null = null;
    const idle = async (id: string) => {
      const deadline = performance.now() + 15000;
      while (true) {
        const observation = await port.readThread(id);
        if (observation.id === id && observation.hostId === 'local' && observation.kind === 'codex' &&
          ['idle', 'notLoaded'].includes(observation.status.type) && observation.status.activeFlags.length === 0) return;
        requireThat(performance.now() < deadline, 'phone_archive_task_active', 'Every affected task must become locally idle before archival.');
        await delay(100);
      }
    };
    const before = async (candidate?: DesktopVoiceCandidate) => {
      const actor = await port.readThread(settings.appTools.actorThreadId);
      requireThat(actor.id === settings.appTools.actorThreadId && actor.hostId === 'local' && actor.kind === 'codex',
        'phone_archive_actor_changed', 'Archive controller no longer belongs to the configured local App.');
      if (candidate) {
        for (const id of [candidate.threadId, ...candidate.descendantIds]) await idle(id);
      }
      await assertIdle(); this.owner(call);
      verifyDesktopArchiveActor(settings.databasePath, settings.appTools.actorThreadId);
      if (candidate) requireThat(canonical(readDesktopVoiceCandidates(settings.databasePath, candidate.threadId)) === canonical([candidate]),
        'phone_archive_scope_changed', 'Original Voice classification or descendant scope changed before submission.');
      if (!submitted) { this.journal.submit(intent); submitted = true; }
    };
    try {
      if (!candidates.length) await before();
      for (let index = 0; index < candidates.length; index++) {
        const candidate = candidates[index]!; failedThreadId = candidate.threadId;
        await port.archiveThread(candidate.threadId, plan.operationIds[index]!, () => before(candidate));
        archivedIds.push(candidate.threadId); failedThreadId = null;
      }
      this.journal.finishArchive(intent, { state: 'archived', archivedIds, failedThreadId: null });
    } catch (error) {
      if (submitted) {
        // A partial batch is never silently reported complete, including a later preflight failure.
        if (this.journal.epoch === call.epoch) this.journal.finishArchive(intent, { state: 'outcome_unknown', archivedIds, failedThreadId });
        throw new IvyError('phone_archive_unknown', 'Original Voice archive is incomplete or unknown; inspect its retained plan and acknowledgements.', 'unknown');
      }
      throw error;
    } finally { await port.close(); }
  }
  /** Explicit reconciliation never submits an archive or repeats an interrupted batch. */
  async reconcile(callId: string, generation = 0): Promise<void> {
    requireThat(Number.isInteger(generation) && generation >= 0 && generation <= 128, 'invalid_arguments', 'Bounded original Voice generation required.');
    requireThat(!this.pending.has(callId), 'phone_archive_in_progress', 'Await original archival before reconciliation.');
    const original = this.journal.callCommand(callId, 'call.archiveVoice', generation);
    requireThat(original?.intent.method === 'call.archiveVoice', 'phone_archive_missing', 'Original archive plan is unavailable.');
    if (original.archiveResolution) return;
    const { settings, candidates } = original.intent.archivePlan;
    const resolved = await this.controller.resolve(settings.databasePath, settings.appTools);
    verifyDesktopArchiveActor(settings.databasePath, resolved.actorThreadId);
    const port = this.open(resolved);
    try {
      const actor = await port.readThread(resolved.actorThreadId);
      requireThat(actor.id === resolved.actorThreadId && actor.kind === 'codex' && actor.hostId === 'local',
        'phone_archive_actor_changed', 'Replacement controller is not available on its owning Desktop.');
      for (const candidate of candidates) {
        for (const id of [candidate.threadId, ...candidate.descendantIds]) {
          const observation = await port.readThread(id);
          requireThat(observation.id === id && observation.hostId === 'local' && observation.kind === 'codex' &&
            ['idle', 'notLoaded'].includes(observation.status.type) && observation.status.activeFlags.length === 0,
          'phone_archive_unconfirmed', 'An original affected task is not confirmed locally idle.');
        }
        requireThat(canonical(readDesktopVoiceCandidates(settings.databasePath, candidate.threadId, true)) === canonical([candidate]),
          'phone_archive_unconfirmed', 'The exact original Voice subtree is not confirmed archived.');
      }
      requireThat(!this.pending.has(callId), 'phone_archive_in_progress', 'Original archival started during reconciliation.');
      this.journal.resolveArchive(original.intent.operationId);
    } finally { await port.close(); }
  }
}
