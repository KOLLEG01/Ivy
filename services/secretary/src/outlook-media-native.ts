import { canonical, deriveOperationId, digest, hashJson, IvyError, NativeOwner, nativeInstant as instant, reconcileNativeOperation, newOperationId } from '../../../packages/sdk/src/node.js';
import type { Agent, Wire } from '../../../packages/sdk/src/node.js';
import { catalogueAccountHash } from './source-identity.js';
import { need, same } from './schema.js';
import type { OutlookSource } from './outlook-source.js';
import { microsoftMethods, validateMicrosoft } from './microsoft-schema.js';
import { microsoftMcp, microsoftProfile } from './outlook-projection.js';
import { outlookAttachments, outlookMaterialization } from './outlook-media.js';
import { validateMedia } from './media-schema.js';
import type { MediaAdmission, MediaAttachment, MediaCoverage, MediaReadCall, MediaReadPlan } from './media-schema.js';
import { MediaSpool } from './media-spool.js';
import { MediaStore, verifyOutlookMediaAdmission } from './media-store.js';
import { MicrosoftEpochBoundaries } from './microsoft-store.js';


interface State { next: number | null; threadId: string | null; attachments: MediaAttachment[]; gap: string | null; download: string | null;
  calls: { slot: number; operationId: string; method: MediaReadCall['method']; contentHash: string }[];
  results: { attachment: MediaAttachment; operationId: string | null; errorCode: string | null }[]; observedAt: string }
const check = (value: unknown, code = 'media_native_evidence_invalid') => need(value, code, 'Native media needs its exact original source, account, call and selected attachment.');
const object = (value: unknown): Record<string, unknown> => { check(value && typeof value === 'object' && !Array.isArray(value)); return value as Record<string, unknown>; };

/** One original native call or binary acquisition per step; publication follows the account gates. */
export class OutlookMediaNative {
  private get boundaries() { return new MicrosoftEpochBoundaries(this.source.engine.store, this.owner, 'secretary/media'); }
  readonly owner: NativeOwner;
  readonly publication: MediaStore;
  constructor(readonly source: OutlookSource, readonly spool: MediaSpool, readonly request: typeof fetch = fetch) {
    this.owner = new NativeOwner(source.engine.client, source.engine.settings.identity.principalId, source.configuration.target, { signal: source.engine.signal });
    this.publication = new MediaStore(source.engine, (admission, captureOperationId) => verifyOutlookMediaAdmission(source, admission, captureOperationId));
  }
  async prepare(admission: MediaAdmission, item: import('./schema.js').Pin, captureOperationId: string): Promise<MediaReadPlan> {
    await verifyOutlookMediaAdmission(this.source, admission, captureOperationId);
    let plan = this.spool.readPlan(item);
    if (!plan) {
      const status = await this.owner.status(), bindings = [];
      for (const method of microsoftMethods) {
        const binding = await this.owner.binding(method);
        bindings.push({ method, definitionHash: binding.definitionHash, outputSchema: {} });
      }
      check((await this.owner.status()).epoch === status.epoch, 'media_native_epoch_changed');
      plan = this.spool.prepareRead({ schemaVersion: 1, admission: structuredClone(admission), item: structuredClone(item), captureOperationId, configuration: this.source.configuration,
        epoch: status.epoch!, createdAt: this.source.engine.now().toISOString(), bindings });
    }
    this.validatePlan(plan); await verifyOutlookMediaAdmission(this.source, plan.admission, plan.captureOperationId); return plan;
  }
  private validatePlan(plan: MediaReadPlan): void {
    validateMedia('MediaReadPlan', plan); validateMicrosoft('OutlookCollector', plan.configuration);
    check(same(plan.configuration, this.source.configuration) && plan.bindings.length === microsoftMethods.length &&
      plan.bindings.every((binding, index) => binding.method === microsoftMethods[index]));
    instant(plan.createdAt); check(plan.admission.itemObjectId === plan.item.objectId && same(this.spool.readPlan(plan.item), plan));
  }
  private async invocation(plan: MediaReadPlan, slot: number, state: State, operationId?: string): Promise<Omit<MediaReadCall, 'preparedAt'>> {
    let method: MediaReadCall['method'], params: Record<string, Wire.Json>;
    if (slot === 0 || slot === 37) { method = 'account/read'; params = { refreshToken: false }; }
    else if (slot === 1) { method = 'thread/start'; params = { cwd: plan.configuration.threadCwd, ephemeral: true, permissions: ':danger-full-access', approvalPolicy: 'never' }; }
    else {
      check(state.threadId);
      if (slot === 38) { method = 'thread/unsubscribe'; params = { threadId: state.threadId }; }
      else {
        method = 'mcpServer/tool/call'; let tool = 'get_profile', args: Record<string, Wire.Json> = {};
        if (slot === 3) { tool = 'list_attachments'; args = { message_id: plan.admission.providerMessageId, content_mode: 'metadata_only' }; }
        else if (slot >= 4 && slot <= 35) { const attachment = state.attachments[slot - 4]; check(attachment);
          tool = 'fetch_attachment'; args = { message_id: plan.admission.providerMessageId, attachment_id: attachment!.attachmentId }; }
        params = { threadId: state.threadId, server: 'codex_apps', tool: 'microsoft_outlook_email.' + tool, arguments: args };
      }
    }
    const planHash = hashJson(plan);
    return { planHash, slot, method, params, operationId: operationId ?? await newOperationId(this.source.engine.client),
      definitionHash: plan.bindings.find(binding => binding.method === method)!.definitionHash };
  }
  private evaluate(plan: MediaReadPlan, state: State, call: MediaReadCall, operation: Agent.Operation): void {
    this.owner.checkOperation(operation, call); check(['succeeded', 'failed'].includes(operation.phase));
    instant(call.preparedAt); check(instant(operation.createdAt) >= instant(call.preparedAt));
    state.observedAt = operation.updatedAt; state.calls.push({ slot: call.slot, operationId: call.operationId, method: call.method, contentHash: digest(canonical(operation, 4 * 1024 * 1024)) });
    try {
      check(operation.phase === 'succeeded' && operation.reply && 'result' in operation.reply, 'media_native_failed');
      if (!operation.reply || !('result' in operation.reply)) throw Error('Unreachable');
      check(call.slot === 38 || operation.epoch === plan.epoch, 'media_native_epoch_changed');
      const result = operation.reply.result;
      if (call.slot === 0 || call.slot === 37) check(catalogueAccountHash(result) === plan.configuration.expectedAccountHash, 'media_account_mismatch');
      else if (call.slot === 1) { const id = object(object(result)['thread'])['id']; check(typeof id === 'string' && id.length > 0); state.threadId = String(id); }
      else if (call.slot === 2 || call.slot === 36) check(same(microsoftProfile(microsoftMcp(result)), plan.configuration.profile), 'media_account_mismatch');
      else if (call.slot === 3) state.attachments = outlookAttachments(microsoftMcp(result), plan.admission.providerMessageId);
      else if (call.slot === 38) check(['unsubscribed', 'notLoaded', 'notSubscribed'].includes(String(object(result)['status'])), 'media_cleanup_failed');
      else {
        const attachment = state.attachments[call.slot - 4]!, materialization = outlookMaterialization(microsoftMcp(result), attachment);
        const operationId = deriveOperationId(call.operationId, 'download');
        const attempt = this.spool.prepare({ operationId, captureOperationId: plan.captureOperationId, item: plan.item, admission: plan.admission, materializationHash: hashJson(materialization) }, materialization);
        if (attempt.phase === 'prepared' || attempt.phase === 'downloading') { state.download = operationId; return; }
        state.results.push({ attachment, operationId, errorCode: attempt.errorCode });
      }
      state.next = call.slot === 38 ? null : call.slot + 1;
    } catch (error) {
      if (!(error instanceof IvyError)) throw error;
      if (call.slot === 38) throw error; // Failed cleanup never certifies a completed read plan.
      if (call.slot >= 4 && call.slot <= 35) { state.results.push({ attachment: state.attachments[call.slot - 4]!, operationId: null, errorCode: error.code }); state.next = call.slot + 1; }
      else { state.gap = state.gap ?? error.code; state.next = state.threadId && call.slot !== 38 ? 38 : null; }
    }
  }
  private async state(plan: MediaReadPlan): Promise<State> {
    const state: State = { next: 0, threadId: null, attachments: [], gap: null, download: null, calls: [], results: [], observedAt: plan.createdAt };
    await verifyOutlookMediaAdmission(this.source, plan.admission, plan.captureOperationId);
    const item = await this.source.engine.item(plan.item.objectId);
    if (item.value.message.nativeRevision.startsWith('outlook-inline-content:')) {
      // The connector's attachment list cannot prove coverage of these original HTML references.
      // Retain an explicit content-bound gap instead of certifying the text projection as complete.
      state.next = null; state.gap = 'media_inline_coverage_unavailable'; return state;
    }
    while (state.next !== null) {
      if (state.next >= 4 && state.next <= 35) {
        const attachment = state.attachments[state.next - 4];
        if (!attachment) { state.next = 36; continue; }
        if (attachment.attachmentType !== 'fileAttachment' || !attachment.payloadFetchSupported) {
          state.results.push({ attachment, operationId: null, errorCode: 'media_attachment_unsupported' }); state.next++; continue;
        }
      }
      const original = await this.spool.resolvedNativeCall(this.source.engine.client,hashJson(plan), state.next); if (!original) break;
      const boundary = await this.boundaries.read(plan.item, original.call, plan.epoch);
      if (boundary) {
        check(!original.seen && !original.observation && state.next !== 38 && boundary.status.observedAt >= state.observedAt);
        state.observedAt = boundary.status.observedAt; state.gap = 'media_native_epoch_changed'; state.next = state.threadId ? 38 : null; continue;
      }
      const { preparedAt: _preparedAt, ...invocation } = original.call; check(same(invocation, await this.invocation(plan, state.next, state, original.call.operationId)));
      if (original.seen) this.owner.checkOperation(original.seen, original.call);
      if (!original.observation) break;
      this.evaluate(plan, state, original.call, original.observation); if (state.download) break;
    }
    return state;
  }
  async verifyCoverage(plan: MediaReadPlan, coverage: MediaCoverage): Promise<void> {
    this.validatePlan(plan); validateMedia('MediaCoverage', coverage); const state = await this.state(plan);
    check(state.next === null && state.download === null && same(coverage.admission, plan.admission) &&
      coverage.observedAt === state.observedAt && coverage.gap === state.gap && coverage.attachments.length === state.attachments.length,
      'media_coverage_invalid');
    for (const [index, attachment] of state.attachments.entries()) {
      const saved = coverage.attachments[index]!, result = state.results.find(value => value.attachment.attachmentId === attachment.attachmentId);
      const errorCode = state.gap ?? result?.errorCode ?? (result?.operationId ? null : 'media_acquisition_incomplete');
      check(same(saved.attachment, attachment) && saved.errorCode === errorCode, 'media_coverage_invalid');
      if (errorCode) check(saved.manifest === null, 'media_coverage_invalid');
      else {
        check(result?.operationId && saved.manifest, 'media_coverage_invalid'); if (!result?.operationId || !saved.manifest) throw Error('Unreachable');
        const attempt = this.spool.get(result.operationId); check(attempt?.phase === 'acquired', 'media_coverage_invalid');
        const manifest = await this.publication.manifest(saved.manifest, plan.admission);
        check(same(manifest.evidence, attempt!.evidence), 'media_coverage_invalid');
      }
    }
  }
  async step(plan: MediaReadPlan): Promise<{ phase: 'pending' } | { phase: 'complete'; coverage: MediaCoverage }> {
    this.validatePlan(plan); await this.source.engine.verifyOwner(); let state = await this.state(plan);
    if (state.download) {
      await this.spool.acquire(state.download, this.source.engine.signal, () => verifyOutlookMediaAdmission(this.source, plan.admission, plan.captureOperationId), this.request);
      return { phase: 'pending' };
    }
    if (state.next !== null) {
      const original = await this.spool.resolvedNativeCall(this.source.engine.client,hashJson(plan), state.next);
      const call = original?.call ?? this.spool.prepareNativeCall({ ...await this.invocation(plan, state.next, state), preparedAt: this.source.engine.now().toISOString() }).call;
      const result = await reconcileNativeOperation({ owner: this.owner, call, absenceConflictCode: 'media_native_absence_conflict',
        journal: { current: null, previous: original?.observation ?? original?.seen ?? null, wasObserved: !!(original?.observation || original?.seen),
          retain: async operation => { this.spool.observeNativeCall(call, operation); return null; } },
        onAbsent: async (invocation, absence) => {
          if (call.slot !== 38 && absence.epoch !== plan.epoch) return this.boundaries.record(plan.item, invocation, plan.epoch, absence, () => this.source.engine.verifyOwner());
          return this.owner.dispatch(invocation, async () => {
            await this.source.engine.verifyOwner(); await verifyOutlookMediaAdmission(this.source, plan.admission, plan.captureOperationId);
            const status = await this.owner.status(); check(status.epoch === absence.epoch && (call.slot === 38 || status.epoch === plan.epoch), 'media_native_epoch_changed');
          });
        } });
      if (result.kind === 'absent') return { phase: 'pending' };
      // Download a newly materialized URL immediately, before unrelated scheduler work can expire it.
      state = await this.state(plan);
      if (state.download) await this.spool.acquire(state.download, this.source.engine.signal, () => verifyOutlookMediaAdmission(this.source, plan.admission, plan.captureOperationId), this.request);
      return { phase: 'pending' };
    }
    const coverage: MediaCoverage = { schemaVersion: 1, admission: plan.admission, observedAt: state.observedAt, gap: state.gap, attachments: [] };
    for (const attachment of state.attachments) {
      const result = state.results.find(value => value.attachment.attachmentId === attachment.attachmentId);
      const errorCode = state.gap ?? result?.errorCode ?? (result?.operationId ? null : 'media_acquisition_incomplete');
      const manifest = !errorCode && result?.operationId ? (await this.publication.publish(this.spool, result.operationId)).pin : null;
      coverage.attachments.push({ attachment, manifest, errorCode });
    }
    validateMedia('MediaCoverage', coverage); return { phase: 'complete', coverage };
  }
}
