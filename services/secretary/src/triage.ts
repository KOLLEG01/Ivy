import { IvyError } from '../../../packages/sdk/src/node.js';
import type { SecretaryEngine } from './engine.js';
import type { Pin } from './schema.js';
import { same } from './schema.js';
import { TriageNative } from './triage-native.js';
import type { TriageWorkDocument } from './triage-native.js';
import { TriageResults } from './triage-results.js';
import { triageCheck as check } from './triage-store.js';
import type { TriageSettings } from './triage-schema.js';
import type { TriageMediaResolver } from './triage-media.js';
import type { TriageDocument } from './triage-store.js';
export interface FollowUpContext {
  schemaVersion: 1;
  itemId: string;
  threadId: string;
  target: TriageSettings['target'];
  model: TriageSettings['model'] | null;
  effort: TriageSettings['effort'];
  threadCwd: string;
  developerInstructions: string;
  itemSnapshot?: string;
  retainedAt: string;
  expiresAt: number;
}
const terminal = new Set(['secretary_triage_native_failed', 'secretary_triage_turn_failed', 'secretary_triage_result_invalid', 'secretary_triage_final_ambiguous', 'secretary_triage_unexpected_action']);
export class SecretaryTriage {
  readonly native: TriageNative;
  private itemCursor: string | undefined;
  private active: Promise<void> | null = null;
  constructor(readonly engine: SecretaryEngine, readonly settings: TriageSettings, mediaResolver?: TriageMediaResolver) { this.native = new TriageNative(engine, settings, mediaResolver); }
  private async finish(work: TriageWorkDocument, value: TriageWorkDocument['value']): Promise<void> {
    try { await this.native.store.amend('secretary/triage-work', work, value); }
    catch (error) { const saved = await this.native.work(work.value.item); if (!saved || !same(saved.value, value)) throw error; }
  }
  private retainFollowUpContext(work: TriageWorkDocument, plan: Awaited<ReturnType<TriageNative['plan']>>, completionPin: Pin): void {
    const completion = this.engine.store.technicalRead<TriageDocument<'secretary/triage-completion'>['value']>('secretary/triage-completion', completionPin);
    const retainedAt = this.engine.now();
    this.engine.store.technicalCreate<FollowUpContext>('secretary/follow-up-context', work.value.item.objectId, {
      schemaVersion: 1,
      itemId: work.value.item.objectId,
      threadId: completion.value.threadId,
      target: plan.value.settings.target,
      model: plan.value.settings.model,
      effort: plan.value.settings.effort,
      threadCwd: plan.value.settings.threadCwd,
      developerInstructions: plan.value.developerInstructions,
      retainedAt: retainedAt.toISOString(),
      expiresAt: retainedAt.getTime() + 90 * 24 * 60 * 60 * 1000
    });
  }
  async step(item: Pin): Promise<'pending' | 'assessed' | 'superseded' | 'failed'> {
    const work = await this.native.prepare(item), plan = await this.native.plan(work), results = new TriageResults(this.native, plan);
    const operationId = work.value.operationId, caller = this.engine.settings.identity.principalId;
    if (['failed', 'superseded'].includes(work.value.phase)) return work.value.phase as 'failed' | 'superseded';
    const current = await this.engine.item(item.objectId), previous = await this.engine.find(caller, operationId);
    if (current.value.decision && !previous) {
      // The other decision wins immediately, but this native reservation lasts until its own work is idle.
      if (!await this.native.releaseClosed(plan)) return 'pending';
      await this.finish(work, { ...work.value, phase: 'superseded', errorCode: 'secretary_triage_item_changed' });
      await this.native.detachPlan(plan); this.engine.store.completeTechnicalWorkflow(work.pin.objectId); return 'superseded';
    }
    let completion = await results.find();
    if (!completion) {
      check(!previous && work.value.phase === 'pending'); await this.native.guard(plan);
      const started = await this.native.beginTurn(plan); if (!started) return 'pending';
      completion = await results.collect(started.turn, started.threadId, started.epoch); if (!completion) return 'pending';
    }
    const request = { action: 'assess' as const, operationId, expectedScope: plan.value.identity.scope, item, assessment: completion.value.result };
    const accepted = await this.engine.action(caller, request);
    if (accepted.phase === 'failed' && accepted.errorCode === 'revision_conflict') {
      if (!await this.native.releaseClosed(plan)) return 'pending';
      await this.finish(work, { ...work.value, phase: 'superseded', completion: completion.pin, errorCode: 'secretary_triage_item_changed' });
      await this.native.detachPlan(plan); this.engine.store.completeTechnicalWorkflow(work.pin.objectId); return 'superseded';
    }
    check(accepted.phase === 'succeeded' && accepted.effect && same(accepted.request, request), 'secretary_triage_assessment_unconfirmed');
    const assessed = await this.engine.item(accepted.effect!);
    check(assessed.value.decision?.actor === caller && assessed.value.decision.operationId === operationId && same(assessed.value.decision.assessment, completion.value.result));
    this.retainFollowUpContext(work, plan, completion.pin);
    await this.finish(work, { ...work.value, phase: 'assessed', completion: completion.pin, assessment: accepted.effect, errorCode: null });
    await this.native.detachPlan(plan); this.engine.store.completeTechnicalWorkflow(work.pin.objectId); return 'assessed';
  }
  tick(): Promise<void> { if (this.active) return this.active; this.active = this.run().finally(() => { this.active = null; }); return this.active; }
  private async advance(work: TriageWorkDocument): Promise<void> {
    const key = 'triage:' + work.pin.objectId;
    // Once admission succeeded, later errors belong to the durable work, not its former queued item.
    this.engine.recoveryIssues.delete('triage:' + work.value.item.objectId);
    try { const result = await this.step(work.value.item); if (result !== 'failed') this.engine.recoveryIssues.delete(key); }
    catch (error) {
      if (this.engine.signal.aborted) throw error; const code = IvyError.from(error).code;
      if (code === 'secretary_owner_mismatch') throw error;
      const current = await this.native.work(work.value.item); check(current);
      if (current!.value.phase !== 'pending') {
        if (current!.value.phase === 'failed') this.engine.issue(key, current!.value.errorCode!);
        else this.engine.recoveryIssues.delete(key);
        return;
      }
      if (code === 'secretary_triage_item_changed') {
        // A concurrent decision can win after step's first read. Keep the native slot until the
        // next step reconciles its original start/turn and cleanup; never execute another assessment.
        this.engine.recoveryIssues.delete(key);
      }
      else {
        if (terminal.has(code)) {
          // Keep the shared thread reserved until this turn's cleanup is confirmed.
          // Marking it failed earlier would let the next message overtake its cleanup.
          try {
            if (!await this.native.releaseClosed(await this.native.plan(current!))) { this.engine.issue(key, code); return; }
          } catch (releaseError) {
            if (this.engine.signal.aborted || IvyError.from(releaseError).code === 'secretary_owner_mismatch') throw releaseError;
            this.engine.issue(key, IvyError.from(releaseError).code); return;
          }
          const plan = await this.native.plan(current!);
          await this.finish(current!, { ...current!.value, phase: 'failed', errorCode: code });
          await this.native.detachPlan(plan); this.engine.store.completeTechnicalWorkflow(current!.pin.objectId);
        }
        this.engine.issue(key, code);
      }
    }
  }
  private async run(): Promise<void> {
    await this.engine.verifyOwner(); const parent = { op: 'eq' as const, field: 'object.parentId', value: this.engine.store.root };
    const workRows = this.engine.store.technicalList<TriageWorkDocument['value']>('secretary/triage-work');
    const failures = workRows.filter(row => ['failed', 'superseded'].includes(row.value.phase)).slice(0, this.engine.settings.recordsPerTick);
    for (const row of failures) {
      const work = await this.native.store.read('secretary/triage-work', row.pin);
      const checked = await this.native.work(work.value.item); check(checked && ['failed', 'superseded'].includes(checked.value.phase));
      if (checked!.value.phase === 'failed') this.engine.issue('triage:' + row.pin.objectId, checked!.value.errorCode!);
      else this.engine.recoveryIssues.delete('triage:' + row.pin.objectId);
      const key = 'triage-release:' + row.pin.objectId;
      try {
        const plan = await this.native.plan(checked!);
        if (await this.native.releaseClosed(plan)) { await this.native.detachPlan(plan); this.engine.store.completeTechnicalWorkflow(checked!.pin.objectId); this.engine.recoveryIssues.delete(key); }
        else this.engine.issue(key, 'secretary_triage_release_pending');
      } catch (error) { if (this.engine.signal.aborted || IvyError.from(error).code === 'secretary_owner_mismatch') throw error; this.engine.issue(key, IvyError.from(error).code); }
    }
    const pending = workRows.filter(row => row.value.phase === 'pending').slice(0, this.engine.settings.recordsPerTick);
    for (const row of pending) await this.advance(await this.native.store.read('secretary/triage-work', row.pin));
    if (workRows.some(row => row.value.phase === 'pending')) return;
    const items = await this.engine.client.request('objects.query', { contractKey: 'secretary/item', limit: this.engine.settings.recordsPerTick,
      ...(this.itemCursor ? { cursor: this.itemCursor } : {}), orderBy: [{ field: 'object.id', direction: 'asc' }], where: { op: 'and', args: [parent, { op: 'isNull', field: 'data:/decision' }] } });
    this.itemCursor = items.nextCursor ?? undefined;
    for (const row of items.items) {
      const item = { objectId: row.objectId, revision: row.revision }; if (await this.native.work(item)) continue;
      try {
        const work = await this.native.prepare(item); await this.advance(work);
        if ((await this.native.work(item))?.value.phase === 'pending') break; // One active turn on the shared thread; drain completed work within the page bound.
      } catch (error) {
        if (this.engine.signal.aborted || IvyError.from(error).code === 'secretary_owner_mismatch') throw error;
        this.engine.issue('triage:' + item.objectId, IvyError.from(error).code);
        // Missing media has admitted no work. Continue this page so another item is not skipped forever.
        // A failure after saving work still occupies capacity and must stop this admission pass.
        if (await this.native.work(item)) break;
      }
    }
  }
}
