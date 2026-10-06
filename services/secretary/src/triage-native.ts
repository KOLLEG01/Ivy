import { ignored } from './policy.js';
import { reconcileNativeOperation, NativeOwner, newOperationId } from '../../../packages/sdk/src/node.js';
import type { Agent, Wire } from '../../../packages/sdk/src/node.js';
import { record } from './native-record.js';
import type { SecretaryEngine } from './engine.js';
import type { Pin } from './schema.js';
import { same } from './schema.js';
import { mutation } from './store.js';
import { triageConfiguration } from './triage-config.js';
import { triageMethods } from './triage-schema.js';
import type { TriageInput, TriageMethod, TriageSettings, TriageParameters } from './triage-schema.js';
import { TriageStore, triageName, triageCheck as check } from './triage-store.js';
import type { TriageDocument } from './triage-store.js';
import { triagePrompt, triageNativeParams, triageToolConfiguration } from './triage-prompt.js';
import { triageMedia } from './triage-media.js';
import type { TriageMediaResolver } from './triage-media.js';

type StoredPlan = TriageDocument<'secretary/triage-plan'>;
export interface TriagePlanDocument { pin: Pin; value: Omit<StoredPlan['value'], 'nativeParams'> & { nativeParams: TriageParameters } }
export type TriageWorkDocument = TriageDocument<'secretary/triage-work'>;
type StoredCall = TriageDocument<'secretary/triage-call'>;
export interface TriageCallDocument {
  pin: Pin;
  stored: StoredCall;
  value: Omit<StoredCall['value'], 'observation'> & { params: Record<string, Wire.Json>; observation: Agent.Operation | null };
}
export class TriageNative {
  readonly store: TriageStore;
  activeThreadId: string | null = null;
  private readonly plans = new Map<string, TriagePlanDocument>();
  constructor(readonly engine: SecretaryEngine, readonly settings: TriageSettings, readonly mediaResolver?: TriageMediaResolver) { triageConfiguration(settings, engine.settings); this.settings = structuredClone(settings); this.store = new TriageStore(engine); }
  owner(plan: TriagePlanDocument) { return new NativeOwner(this.engine.client, this.engine.settings.identity.principalId, plan.value.settings.target, { signal: this.engine.signal }); }
  async work(item: Pin): Promise<TriageWorkDocument | null> {
    const work = await this.store.find('secretary/triage-work', triageName('work', item)); if (!work) return null;
    check(same(work.value.item, item));
    if (work.value.phase === 'pending') check(work.value.completion === null && work.value.assessment === null && work.value.errorCode === null);
    else if (work.value.phase === 'assessed') check(work.value.completion && work.value.assessment && work.value.errorCode === null);
    else check(work.value.errorCode !== null && work.value.assessment === null);
    return work;
  }
  async plan(work: TriageWorkDocument): Promise<TriagePlanDocument> {
    const key = work.value.plan.objectId + ':' + work.value.plan.revision, cached = this.plans.get(key);
    if (cached) return cached;
    const plan = await this.store.read('secretary/triage-plan', work.value.plan, triageName('plan', work.value.item), true), value = plan.value;
    const storedParams = record(await this.store.nativeJson('parameters', work.value.item, value.nativeParams));
    const nativeParams = record(storedParams['params']) as unknown as TriageParameters;
    const result = { pin: plan.pin, value: { ...value, nativeParams } };
    if (this.plans.size >= 2) this.plans.delete(this.plans.keys().next().value!);
    this.plans.set(key, result); return result;
  }
  private async mediaInput(item: Awaited<ReturnType<SecretaryEngine['item']>>) {
    if (item.value.message.attachments === 'none') return { media: null, inputs: [] };
    const context = await this.mediaResolver?.(item.value.sourceId, item.pin);
    check(context, 'secretary_triage_media_pending'); return triageMedia(context!, item.pin, item.value.sourceId);
  }
  async guard(plan: TriagePlanDocument, cleanup = false): Promise<void> {
    await this.engine.verifyOwner(); const work = await this.work(plan.value.input.item), item = await this.engine.item(plan.value.input.item.objectId);
    check(work && same(work.value.plan, plan.pin) && (cleanup || work.value.phase === 'pending' && same(item.pin, plan.value.input.item) && !item.value.decision), 'secretary_triage_item_changed');
  }
  async prepare(itemPin: Pin): Promise<TriageWorkDocument> {
    await this.engine.verifyOwner(); const prior = await this.work(itemPin); if (prior) { await this.plan(prior); return prior; }
    const item = await this.engine.item(itemPin.objectId); check(same(item.pin, itemPin) && !item.value.decision, 'secretary_triage_item_changed');
    let plan = await this.store.find('secretary/triage-plan', triageName('plan', itemPin), true);
    if (!plan) {
      const media = await this.mediaInput(item), input: TriageInput = { item: itemPin, source: this.engine.sourceDefinition(item.value.sourceId), message: item.value.message,
        contentScope: media.media ? 'verified_media' : 'captured_text_or_caption', media: media.media, policy: this.engine.settings.policy };
      const owner = new NativeOwner(this.engine.client, this.engine.settings.identity.principalId, this.settings.target, { signal: this.engine.signal }), bindings = [];
      // The registered binding shape carries an outputSchema; {} adds no duplicate consumer validation.
      for (const method of triageMethods) { const binding = await owner.binding(method); bindings.push({ method, definitionHash: binding.definitionHash, outputSchema: {} }); }
      const prompt = triagePrompt(input, this.settings), params: TriageParameters = triageNativeParams(this.settings, prompt, media.inputs);
      params.threadStart = await owner.threadStartParams(params.threadStart);
      const nativeParams = await this.store.saveNativeJson('parameters', itemPin, { schemaVersion: 1, item: itemPin, params });
      plan = await this.store.create('secretary/triage-plan', triageName('plan', itemPin), { schemaVersion: 1, identity: this.engine.settings.identity, settings: this.settings, input,
        ...prompt, nativeParams, bindings, createdAt: this.engine.now().toISOString() }, true);
    }
    const work = await this.store.create('secretary/triage-work', triageName('work', itemPin), { schemaVersion: 1, item: itemPin, plan: plan.pin, operationId: await newOperationId(this.engine.client), phase: 'pending', completion: null, assessment: null, errorCode: null });
    await this.plan(work); return work;
  }
  params(plan: TriagePlanDocument, method: TriageMethod, threadId?: string): Record<string, Wire.Json> {
    if (method === 'thread/start') return structuredClone(plan.value.nativeParams.threadStart);
    check(threadId, 'secretary_triage_thread_missing');
    if (method === 'thread/resume') return { ...structuredClone(plan.value.nativeParams.threadResume), threadId: threadId! };
    if (method === 'thread/unsubscribe') return { threadId: threadId! };
    return { ...structuredClone(plan.value.nativeParams.turnStart), threadId: threadId! };
  }
  async call(plan: TriagePlanDocument, slot: string, method: TriageMethod, threadId?: string, create = true): Promise<TriageCallDocument> {
    const name = triageName('call', [plan.pin, slot]), params = this.params(plan, method, threadId), definitionHash = plan.value.bindings.find(binding => binding.method === method)!.definitionHash;
    let operationId: string;
    let call = await this.store.find('secretary/triage-call', name);
    if (!call) {
      operationId = await newOperationId(this.engine.client);
      const identity = [plan.pin, slot], expected = { schemaVersion: 1, plan: plan.pin, slot, method, operationId, definitionHash, params };
      check(create, 'secretary_triage_call_missing'); await this.guard(plan, method === 'thread/unsubscribe');
      const request = await this.store.saveNativeJson('request', identity, expected);
      await this.guard(plan, method === 'thread/unsubscribe');
      call = await this.store.create('secretary/triage-call', name, { schemaVersion: 1, plan: plan.pin, slot, method, request, operationId, definitionHash, preparedAt: this.engine.now().toISOString(), observation: null });
    } else operationId = call.value.operationId;
    const hydrated = await this.hydrate(plan, call, params);
    return hydrated;
  }
  private async hydrate(plan: TriagePlanDocument, stored: StoredCall, params: Record<string, Wire.Json>): Promise<TriageCallDocument> {
    const observation = stored.value.observation ? await this.store.nativeJson('operation', [plan.pin, stored.value.slot], stored.value.observation) as Agent.Operation : null;
    const call = { pin: stored.pin, stored, value: { ...stored.value, params, observation } };
    if (observation) this.owner(plan).checkOperation(observation, call.value); return call;
  }
  private async retainObservation(plan: TriagePlanDocument, call: TriageCallDocument, observed: Agent.Operation, cleanup: boolean): Promise<TriageCallDocument> {
    await this.guard(plan, cleanup);
    const observation = await this.store.saveNativeJson('operation', [plan.pin, call.value.slot], observed);
    await this.guard(plan, cleanup);
    const stored = await this.store.amend('secretary/triage-call', call.stored, { ...call.stored.value, observation });
    return this.hydrate(plan, stored, call.value.params);
  }
  async advance(plan: TriagePlanDocument, call: TriageCallDocument, beforeDispatch?: (status: Agent.Status) => Promise<void>): Promise<TriageCallDocument> {
    const owner = this.owner(plan), cleanup = call.value.method === 'thread/unsubscribe';
    const result = await reconcileNativeOperation({ owner, call: call.value, absenceConflictCode: 'secretary_triage_absence_conflict',
      journal: { current: call, previous: call.value.observation, wasObserved: call.value.observation !== null,
        retain: observed => this.retainObservation(plan, call, observed, cleanup) },
      onAbsent: async (original, absence) => {
        const status = await owner.status(); check(status.epoch === absence.epoch, 'secretary_triage_epoch_changed');
        if (beforeDispatch) await beforeDispatch(status);
        return owner.dispatch(original, async () => {
          await this.guard(plan, cleanup);
          if (!cleanup) {
            const params = plan.value.nativeParams;
            check([params.threadStart, params.threadResume, params.turnStart].every(value => value['permissions'] === ':read-only' && value['approvalPolicy'] === 'never') &&
              [params.threadStart, params.threadResume].every(value => same(value['config'], triageToolConfiguration)) &&
              params.threadStart['baseInstructions'] === undefined, 'secretary_triage_execution_policy_changed');
            const source = this.engine.settings.sources.find(source => source.sourceId === plan.value.input.source.sourceId);
            check(source && !ignored(source, plan.value.input.message), 'secretary_triage_authorization_changed');
          }
        });
      } });
    check(result.kind === 'observed', 'secretary_triage_outcome_unavailable'); return result.value;
  }
  /** Closed work may observe its original start, but cannot dispatch a previously prepared start. */
  private async observeClosed(plan: TriagePlanDocument, call: TriageCallDocument): Promise<TriageCallDocument | null> {
    await this.guard(plan, true);
    const result = await reconcileNativeOperation({ owner: this.owner(plan), call: call.value, absenceConflictCode: 'secretary_triage_absence_conflict',
      journal: { current: call, previous: call.value.observation, wasObserved: call.value.observation !== null,
        retain: observed => this.retainObservation(plan, call, observed, true) } });
    return result.kind === 'observed' ? result.value : null;
  }
  threadId(call: TriageCallDocument): string | null {
    if (call.value.observation?.phase !== 'succeeded') return null;
    const reply = call.value.observation.reply; check(reply && 'result' in reply); const result = record(reply && 'result' in reply ? reply.result : null), thread = record(result['thread']);
    check(typeof thread['id'] === 'string' && thread['id'] && thread['ephemeral'] === false, 'secretary_triage_thread_missing');
    check(result['model'] === call.value.params['model'], 'secretary_triage_model_changed'); return thread['id'] as string;
  }
  pending(call: TriageCallDocument): void { check(call.value.observation?.phase !== 'failed', 'secretary_triage_native_failed'); check(call.value.observation?.phase !== 'outcome_unknown', 'secretary_triage_outcome_unknown'); }
  released(call: TriageCallDocument): boolean {
    if (call.value.observation?.phase !== 'succeeded') { this.pending(call); return false; }
    const reply = call.value.observation.reply;
    check(reply && 'result' in reply && ['unsubscribed', 'notLoaded', 'notSubscribed'].includes(String(record(reply && 'result' in reply ? reply.result : null)['status'])), 'secretary_triage_cleanup_failed'); return true;
  }
  async releaseClosed(plan: TriagePlanDocument): Promise<boolean> {
    const original = await this.store.find('secretary/triage-call', triageName('call', [plan.pin, 'thread'])); if (!original) { await this.releaseUnstarted(plan); return true; }
    const start = await this.observeClosed(plan, await this.threadCall(plan, original));
    if (!start) { await this.releaseUnstarted(plan); return true; }
    if (start.value.observation?.phase === 'failed') { await this.releaseUnstarted(plan); return true; }
    const threadId = this.threadId(start); if (!threadId) { this.pending(start); return false; }
    await this.rememberThread(plan, threadId);
    const cleanup = await this.store.find('secretary/triage-call', triageName('call', [plan.pin, 'cleanup']));
    if (cleanup) return this.released(await this.advance(plan, await this.call(plan, 'cleanup', 'thread/unsubscribe', threadId, false)));
    const pendingTurn = await this.store.find('secretary/triage-call', triageName('call', [plan.pin, 'turn']));
    let turn: TriageCallDocument | null = null;
    if (pendingTurn) {
      turn = await this.observeClosed(plan, await this.call(plan, 'turn', 'turn/start', threadId, false));
      if (turn && (!turn.value.observation || !['succeeded', 'failed'].includes(turn.value.observation.phase))) { this.pending(turn); return false; }
    }
    const owner = this.owner(plan), status = await owner.status(), read = await owner.read('thread/read', { threadId, includeTurns: false }, status.epoch!);
    const { thread, canStartTurn } = await owner.threadState(read, status.epoch!);
    check(thread['id'] === threadId && thread['ephemeral'] === false, 'secretary_triage_thread_changed');
    if (!canStartTurn) return false;
    if (turn?.value.observation?.phase === 'succeeded') {
      const reply = turn.value.observation.reply, expected = record(record(reply && 'result' in reply ? reply.result : null)['turn'])['id'];
      const last = await owner.read('thread/turns/list', { threadId, cursor: null, limit: 1, sortDirection: 'desc', itemsView: 'notLoaded' }, status.epoch!);
      const result = record('result' in last.reply ? last.reply.result : null), rows = result['data'];
      check(typeof expected === 'string' && Array.isArray(rows) && rows.length === 1, 'secretary_triage_turn_changed');
      const observed = record((rows as unknown[])[0]); check(observed['id'] === expected, 'secretary_triage_turn_changed');
      if (!['completed', 'failed', 'interrupted'].includes(String(observed['status']))) return false;
    }
    return this.released(await this.advance(plan, await this.call(plan, 'cleanup', 'thread/unsubscribe', threadId), async current => { check(current.epoch === status.epoch, 'secretary_triage_epoch_changed'); }));
  }
  private threadName(plan: TriagePlanDocument) { return triageName('shared-thread', [plan.value.settings.target.serviceNodeId, plan.value.settings.threadCwd]); }
  private async sharedThread(plan: TriagePlanDocument) {
    const name = this.threadName(plan);
    const saved = await this.store.find('secretary/triage-thread', name);
    if (saved) return saved.value.plan ? saved : this.store.amend('secretary/triage-thread', saved, { ...saved.value, plan: plan.pin });
    return this.store.create('secretary/triage-thread', name, { schemaVersion: 1, plan: plan.pin, threadId: null });
  }
  private async releaseUnstarted(plan: TriagePlanDocument) {
    const shared = await this.store.find('secretary/triage-thread', this.threadName(plan));
    if (shared && !shared.value.threadId && same(shared.value.plan, plan.pin)) await this.store.amend('secretary/triage-thread', shared, { ...shared.value, plan: null });
  }
  async detachPlan(plan: TriagePlanDocument): Promise<void> {
    const shared = await this.store.find('secretary/triage-thread', this.threadName(plan));
    if (shared && same(shared.value.plan, plan.pin)) await this.store.amend('secretary/triage-thread', shared, { ...shared.value, plan: null });
  }
  private async rememberThread(plan: TriagePlanDocument, threadId: string) {
    const shared = await this.sharedThread(plan);
    if (!shared.value.threadId) await this.store.amend('secretary/triage-thread', shared, { ...shared.value, threadId });
    // Completing a retained call never replaces the selected shared thread.
  }
  private async threadCall(plan: TriagePlanDocument, saved: StoredCall) {
    const request = record(await this.store.nativeJson('request', [plan.pin, 'thread'], saved.value.request));
    const params = record(request['params']);
    return this.call(plan, 'thread', saved.value.method, typeof params['threadId'] === 'string' ? params['threadId'] : undefined, false);
  }
  async attached(plan: TriagePlanDocument, start: TriageCallDocument, threadId: string, epoch: string): Promise<boolean> {
    if (start.value.observation?.epoch === epoch) return true;
    const resumed = await this.advance(plan, await this.call(plan, 'resume:' + epoch, 'thread/resume', threadId), async status => { check(status.epoch === epoch, 'secretary_triage_epoch_changed'); });
    const id = this.threadId(resumed); if (!id) { this.pending(resumed); return false; }
    check(id === threadId && resumed.value.observation?.epoch === epoch, 'secretary_triage_thread_changed'); return true;
  }
  async beginTurn(plan: TriagePlanDocument): Promise<{ turn: TriageCallDocument; threadId: string; epoch: string } | null> {
    const existing = await this.store.find('secretary/triage-call', triageName('call', [plan.pin, 'thread']));
    const shared = await this.sharedThread(plan);
    if (!existing && !shared.value.threadId && !same(shared.value.plan, plan.pin)) return null;
    const call = existing ? await this.threadCall(plan, existing) : await this.call(plan, 'thread', shared.value.threadId ? 'thread/resume' : 'thread/start', shared.value.threadId ?? undefined);
    const start = await this.advance(plan, call), threadId = this.threadId(start);
    if (!threadId) { this.pending(start); return null; }
    this.activeThreadId = threadId;
    await this.rememberThread(plan, threadId);
    const turn = await this.advance(plan, await this.call(plan, 'turn', 'turn/start', threadId), async status => {
      check(await this.attached(plan, start, threadId, status.epoch!), 'secretary_triage_attachment_pending');
      const owner = this.owner(plan), read = await owner.read('thread/read', { threadId, includeTurns: false }, status.epoch!);
      const { thread, canStartTurn } = await owner.threadState(read, status.epoch!);
      check(thread['id'] === threadId && thread['ephemeral'] === false && canStartTurn, 'secretary_triage_thread_busy');
    });
    if (turn.value.observation?.phase !== 'succeeded') { this.pending(turn); return null; }
    const status = await this.owner(plan).status(); if (!await this.attached(plan, start, threadId, status.epoch!)) return null;
    return { turn, threadId, epoch: status.epoch! };
  }
}
