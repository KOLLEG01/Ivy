import { sourceIdentity } from './engine.js';
import { isAbsolute } from 'node:path';
import { hashJson, IvyError, NativeOwner, nativeInstant as instant, reconcileNativeOperation, newOperationId } from '../../../packages/sdk/src/node.js';
import type { Agent, Wire } from '../../../packages/sdk/src/node.js';
import { catalogueAccountHash } from './source-identity.js';
import { same, need } from './schema.js';
import type { Pin } from './schema.js';
import type { SecretaryEngine } from './engine.js';
import { mutation } from './store.js';
import { MicrosoftEpochBoundaries, MicrosoftStore } from './microsoft-store.js';
import type { MicrosoftDocument } from './microsoft-store.js';
import { microsoftMethods, validateMicrosoft } from './microsoft-schema.js';
import type { MicrosoftBinding, OutlookCall, OutlookCollector, OutlookEvidence, OutlookHead, OutlookPage, MicrosoftMethod } from './microsoft-schema.js';
import { microsoftMcp, microsoftProfile, outlookArguments, outlookPage } from './outlook-projection.js';


export type OutlookPlanDocument = MicrosoftDocument<'secretary/outlook-plan'>;
export type OutlookHeadDocument = MicrosoftDocument<'secretary/outlook-head'>;
type Call = MicrosoftDocument<'secretary/outlook-call'>;
type State = { next: number | null; threadId: string | null; gap: string | null; retryAfterSeconds: number | null; page: OutlookPage | null; calls: Pin[]; observedAt: string };
const check = (condition: unknown, code = 'microsoft_evidence_invalid') => need(condition, code, 'Native Outlook work requires its original account, source, calls and selected page.');
export const outlookName = (kind: string, value: unknown) => 'outlook-' + kind + '-' + hashJson(value).slice(7);
const object = (value: unknown): Record<string, unknown> => { check(value && typeof value === 'object' && !Array.isArray(value)); return value as Record<string, unknown>; };

/** Native collection is distinct from Secretary capture/checkpoint publication. */
export class OutlookNative {
  readonly owner: NativeOwner;
  readonly store: MicrosoftStore;
  private get boundaries() { return new MicrosoftEpochBoundaries(this.engine.store, this.owner, 'secretary/microsoft'); }
  constructor(readonly engine: SecretaryEngine, readonly configuration: OutlookCollector) {
    validateMicrosoft('OutlookCollector', configuration); this.configuration = structuredClone(configuration);
    const source = engine.sourceDefinition(configuration.sourceId);
    check(source.kind === 'email' && source.accountId === configuration.profile.id && source.producerPrincipalIds.includes(engine.settings.identity.principalId), 'microsoft_source_config_invalid');
    check(isAbsolute(configuration.threadCwd) && same(microsoftProfile(configuration.profile), configuration.profile), 'microsoft_source_config_invalid'); instant(configuration.since);
    this.owner = new NativeOwner(engine.client, engine.settings.identity.principalId, configuration.target, { signal: engine.signal });
    this.store = new MicrosoftStore(engine.store, () => engine.verifyOwner());
  }
  async rawHead(): Promise<OutlookHeadDocument> {
    const name = outlookName('head', this.configuration.sourceId), source = this.engine.sourceDefinition(this.configuration.sourceId);
    const head = await this.store.create('secretary/outlook-head', name, { schemaVersion: 1, configuration: this.configuration, source, pending: null, lastPage: null, continuation: null, nextPollAt: null, lastCompleteAt: null });
    check(same(head.value.configuration, this.configuration) && same(sourceIdentity(head.value.source), sourceIdentity(source)), 'microsoft_source_config_invalid'); return head;
  }
  async plan(pin: Pin): Promise<OutlookPlanDocument> {
    const plan = await this.store.read('secretary/outlook-plan', pin, true), value = plan.value;
    check(same(value.configuration, this.configuration) && same(sourceIdentity(value.source), sourceIdentity(this.engine.sourceDefinition(this.configuration.sourceId))), 'microsoft_source_config_invalid');
    check(value.pageNumber <= this.configuration.maximumPages);
    check(value.bindings.length === microsoftMethods.length && value.bindings.every((binding, index) => binding.method === microsoftMethods[index])); instant(value.preparedAt); outlookArguments(value.selection);
    check(same((await this.store.find('secretary/outlook-plan', outlookName('plan', value.head), true))?.pin, pin)); return plan;
  }
  async guard(plan: OutlookPlanDocument): Promise<void> {
    await this.engine.verifyOwner(); const head = await this.rawHead(); check(same(head.value.pending, plan.pin), 'microsoft_plan_changed');
    const source = await this.engine.source(this.configuration.sourceId);
    check(source.pin.objectId === plan.value.sourceCheckpoint.objectId && source.value.cursor === plan.value.previousCursor, 'microsoft_source_cursor_changed');
  }
  async prepare(head: OutlookHeadDocument): Promise<OutlookPlanDocument | null> {
    if (head.value.pending) return this.plan(head.value.pending);
    const preparedAt = this.engine.now().toISOString(); if (head.value.nextPollAt && preparedAt < head.value.nextPollAt) return null;
    if (!head.value.continuation && this.configuration.since >= preparedAt) return null;
    let plan = await this.store.find('secretary/outlook-plan', outlookName('plan', head.pin), true);
    if (!plan) {
      const status = await this.owner.status(), bindings: MicrosoftBinding[] = [], source = await this.engine.source(this.configuration.sourceId);
      for (const method of microsoftMethods) { const binding = await this.owner.binding(method); bindings.push({ method, definitionHash: binding.definitionHash, outputSchema: {} }); }
      check((await this.owner.status()).epoch === status.epoch, 'microsoft_epoch_changed');
      plan = await this.store.create('secretary/outlook-plan', outlookName('plan', head.pin), { schemaVersion: 1, head: head.pin, configuration: this.configuration, source: source.value.definition,
        sourceCheckpoint: source.pin, previousCursor: source.value.cursor, selection: head.value.continuation?.selection ?? { since: this.configuration.since, until: preparedAt, skip: 0, pageSize: this.configuration.pageSize },
        pageNumber: head.value.continuation?.pageNumber ?? 1, epoch: status.epoch!, preparedAt, bindings }, true);
    }
    plan = await this.plan(plan.pin);
    try { await this.store.amend('secretary/outlook-head', head, { ...head.value, pending: plan.pin }); }
    catch (error) { if (!same((await this.rawHead()).value.pending, plan.pin)) throw error; }
    await this.guard(plan); return plan;
  }
  private async invocation(plan: OutlookPlanDocument, slot: number, threadId: string | null, operationId?: string): Promise<Pick<OutlookCall, 'method' | 'params' | 'operationId' | 'definitionHash'>> {
    let method: MicrosoftMethod, params: Record<string, Wire.Json>;
    if (slot === 0 || slot === 5) { method = 'account/read'; params = { refreshToken: false }; }
    else if (slot === 1) { method = 'thread/start'; params = { cwd: this.configuration.threadCwd, ephemeral: true, permissions: ':danger-full-access', approvalPolicy: 'never' }; }
    else {
      check(threadId); if (slot === 6) { method = 'thread/unsubscribe'; params = { threadId }; }
      else { method = 'mcpServer/tool/call'; params = { threadId, server: 'codex_apps', tool: 'microsoft_outlook_email.' + (slot === 3 ? 'list_messages' : 'get_profile'), arguments: slot === 3 ? outlookArguments(plan.value.selection) : {} }; }
    }
    return { method, params, operationId: operationId ?? await newOperationId(this.engine.client), definitionHash: plan.value.bindings.find(binding => binding.method === method)!.definitionHash };
  }
  private async call(plan: OutlookPlanDocument, slot: number, threadId: string | null): Promise<Call | null> {
    const call = await this.store.find('secretary/outlook-call', outlookName('call', [plan.pin, slot])); if (!call) return null;
    await this.store.checkCallHistory('secretary/outlook-call', call, this.owner);
    check(same(call.value.plan, plan.pin) && call.value.slot === slot &&
      same(await this.invocation(plan, slot, threadId, call.value.operationId), { method: call.value.method, params: call.value.params, operationId: call.value.operationId, definitionHash: call.value.definitionHash }));
    check(instant(call.value.preparedAt) >= instant(plan.value.preparedAt)); return call;
  }
  private evaluate(plan: OutlookPlanDocument, state: State, call: Call, operation: Agent.Operation): void {
    this.owner.checkOperation(operation, call.value); check(['succeeded', 'failed'].includes(operation.phase) && instant(operation.createdAt) >= instant(call.value.preparedAt) && call.value.preparedAt >= state.observedAt);
    const slot = call.value.slot; state.observedAt = operation.updatedAt; state.calls.push(call.pin);
    if (slot === 6) {
      check(operation.phase === 'succeeded' && operation.reply && 'result' in operation.reply, 'microsoft_cleanup_failed');
      const result = operation.reply && 'result' in operation.reply ? operation.reply.result : null;
      check(['unsubscribed', 'notLoaded', 'notSubscribed'].includes(String(object(result)['status'])), 'microsoft_cleanup_failed'); state.next = null; return;
    }
    try {
      check(operation.phase === 'succeeded' && operation.reply && 'result' in operation.reply, 'microsoft_native_failed'); check(operation.epoch === plan.value.epoch, 'microsoft_epoch_changed');
      const result = operation.reply && 'result' in operation.reply ? operation.reply.result : null;
      if (slot === 0 || slot === 5) check(catalogueAccountHash(result) === this.configuration.expectedAccountHash, 'microsoft_account_mismatch');
      else if (slot === 1) { const id = object(object(result)['thread'])['id']; check(typeof id === 'string' && id.length > 0); state.threadId = String(id); }
      else if (slot === 2 || slot === 4) check(same(microsoftProfile(microsoftMcp(result)), this.configuration.profile), 'microsoft_account_mismatch');
      else {
        state.page = outlookPage(microsoftMcp(result), this.configuration.profile, plan.value.source, plan.value.selection, operation.updatedAt);
        check(state.page.complete || plan.value.pageNumber < this.configuration.maximumPages, 'microsoft_scan_limit');
      }
      state.next = slot + 1;
    } catch (error) {
      if (!(error instanceof IvyError)) throw error;
      state.gap = error.code; state.retryAfterSeconds = error.code === 'microsoft_rate_limited' ? Number(object(error.details)['retryAfterSeconds']) : null; state.page = null; state.next = state.threadId ? 6 : null;
    }
  }
  private async state(plan: OutlookPlanDocument): Promise<State> {
    const state: State = { next: 0, threadId: null, gap: null, retryAfterSeconds: null, page: null, calls: [], observedAt: plan.value.preparedAt };
    while (state.next !== null) {
      const call = await this.call(plan, state.next, state.threadId); if (!call) break;
      const boundary = await this.boundaries.read(plan.pin, call.value, plan.value.epoch);
      if (boundary) {
        check(!call.value.seen && !call.value.observation && state.next !== 6 && boundary.status.observedAt >= state.observedAt);
        state.calls.push(call.pin); state.observedAt = boundary.status.observedAt; state.gap = 'microsoft_epoch_changed'; state.page = null;
        state.next = state.threadId ? 6 : null; continue;
      }
      if (!call.value.observation) break;
      this.evaluate(plan, state, call, await this.store.operation(plan.pin, call.value.slot, call.value.observation));
    } return state;
  }
  private evidenceValue(plan: OutlookPlanDocument, state: State): OutlookEvidence { return { schemaVersion: 1, plan: plan.pin, calls: state.calls, observedAt: state.observedAt, gap: state.gap, retryAfterSeconds: state.retryAfterSeconds, page: state.page }; }
  async evidence(pin: Pin): Promise<MicrosoftDocument<'secretary/outlook-evidence'>> {
    const proof = await this.store.read('secretary/outlook-evidence', pin, true), plan = await this.plan(proof.value.plan), state = await this.state(plan);
    check(state.next === null && same(proof.value, this.evidenceValue(plan, state)) && same((await this.store.find('secretary/outlook-evidence', outlookName('evidence', plan.pin), true))?.pin, pin)); return proof;
  }
  async collect(plan: OutlookPlanDocument): Promise<MicrosoftDocument<'secretary/outlook-evidence'> | null> {
    plan = await this.plan(plan.pin); const state = await this.state(plan);
    while (state.next !== null) {
      const slot = state.next; let call = await this.call(plan, slot, state.threadId);
      if (!call) {
        await this.guard(plan); await this.store.create('secretary/outlook-call', outlookName('call', [plan.pin, slot]), { schemaVersion: 1, plan: plan.pin, slot, ...await this.invocation(plan, slot, state.threadId), preparedAt: this.engine.now().toISOString(), seen: null, observation: null });
        call = await this.call(plan, slot, state.threadId); check(call);
      }
      if (!call) throw Error('Unreachable'); const retained = call, previous = retained.value.observation ?? retained.value.seen;
      const result = await reconcileNativeOperation({ owner: this.owner, call: retained.value, absenceConflictCode: 'microsoft_native_absence_conflict',
        journal: { current: retained, previous: previous ? await this.store.operation(plan.pin, slot, previous) : null, wasObserved: previous !== null,
          retain: async operation => {
            const terminal = ['succeeded', 'failed'].includes(operation.phase);
            const evidence = await this.store.saveOperation(plan.pin, slot, operation); await this.guard(plan);
            return this.store.amend('secretary/outlook-call', retained, { ...retained.value, ...(terminal ? { observation: evidence } : { seen: evidence }) });
          } },
        onAbsent: async (original, absence) => {
          if (slot !== 6 && absence.epoch !== plan.value.epoch) return this.boundaries.record(plan.pin, original, plan.value.epoch, absence, () => this.guard(plan));
          return this.owner.dispatch(original, async () => {
          await this.guard(plan); const status = await this.owner.status();
          check(status.epoch === absence.epoch && (slot === 6 || status.epoch === plan.value.epoch), 'microsoft_epoch_changed');
          });
        } });
      if (result.kind === 'absent' || !['succeeded', 'failed'].includes(result.observation.phase)) return null;
      call = result.value; this.evaluate(plan, state, call, result.observation);
    }
    await this.guard(plan); const proof = await this.store.create('secretary/outlook-evidence', outlookName('evidence', plan.pin), this.evidenceValue(plan, state), true); return this.evidence(proof.pin);
  }
  nextHead(head: OutlookHead, plan: OutlookPlanDocument, proof: MicrosoftDocument<'secretary/outlook-evidence'>): OutlookHead {
    const value = proof.value, page = value.page; check(Boolean(value.gap) === !page);
    const nextPollAt = page && !page.complete ? value.observedAt : new Date(instant(value.observedAt) + Math.max(this.configuration.pollMs, (value.retryAfterSeconds ?? 0) * 1000)).toISOString();
    return { ...head, pending: null, lastPage: proof.pin, continuation: page && !page.complete ? { selection: { ...plan.value.selection, skip: page.nextIndex! }, pageNumber: plan.value.pageNumber + 1 } : null,
      nextPollAt, lastCompleteAt: page?.complete ? value.observedAt : head.lastCompleteAt };
  }
}
