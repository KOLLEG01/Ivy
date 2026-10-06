import { randomUUID } from 'node:crypto';
import {
  deriveOperationId,
  hashJson,
  IvyError,
  connectionOwner,
  parseOperationId,
  queryDocument,
  runtimeEpoch,
  mainChat,
} from '../../../packages/sdk/src/node.js';
import type { RpcClient, Wire } from '../../../packages/sdk/src/node.js';
import { need, same, validate } from './schema.js';
import type { Accepted, Assessment, AssignmentEntryResponse, AssignmentPage, GetAssignmentRequest, Item, ItemMedia, ListAssignmentsRequest, Message, OperationOutcome, Pin, Policy, Request, SaveAssignmentRequest, SaveConfigurationRequest, Scope, Settings, SourceDefinition, Status } from './schema.js';
import { assignmentName, unifiedMainRules, unifiedRuleOverrides, validateAssignment, validateConfiguration } from './assignment-schema.js';
import { SecretaryStore, conflict, mutation } from './store.js';
import type { Document } from './store.js';
import { checkPolicy, ignored, suppression } from './policy.js';
import { discoverVoiceTarget } from './voice-target.js';

export const operationName = (caller: string, id: string) => 'operation-' + hashJson([caller, id]).slice(7);
const workflowOperationKey = 'secretary/public-operation';
interface WorkflowOperation { operationId: string; callerPrincipalId: string; action: 'create_assignment' | 'update_assignment' | 'configure_execution'; requestHash: string; write: Wire.ObjectWrite; phase: 'prepared' | 'failed'; errorCode: string | null; expiresAt: number }
export const sourceName = (id: string) => 'source-' + hashJson(id).slice(7);
export const messageHash = ({ observedAt: _observedAt, ...message }: Message) => hashJson(message);
export const identityHash = (sourceId: string, message: Message) => hashJson([sourceId, message.accountId, message.conversationId, message.messageId, message.nativeRevision]);
export const noticeText = (assessment: Assessment, message: Message, source: SourceDefinition) => {
  const internalTeamsSender = /^(user|application):/u.test(message.senderId);
  const sender = source.kind === 'email' ? message.senderId : internalTeamsSender ? message.title || 'Microsoft Teams' : message.senderId || message.title;
  const subject = source.kind === 'email' && message.title ? ` – ${message.title}` : '';
  const body = source.kind === 'email' ? assessment.summary : message.text || assessment.summary;
  return `[${message.accountId}] ${sender}${subject}: ${body}`.slice(0, 8192);
};
const immutable = ({ phase: _phase, effect: _effect, errorCode: _errorCode, ...rest }: Accepted) => rest;
const itemIdentity = ({ decision: _decision, notice: _notice, media: _media, ...rest }: Item) => rest;
export const sourceIdentity = ({ sourceId, accountId, kind }: SourceDefinition) => ({ sourceId, accountId, kind });

export class SecretaryEngine {
  async noticeTarget(): Promise<import('./schema.js').NoticeTarget | null> {
    const target = await mainChat(this.client);
    need(target.expectedBridge.callerPrincipalId === this.settings.identity.principalId, 'secretary_notice_unavailable', 'Main must identify Secretary as the actual caller.');
    return target;
  }
  readonly store: SecretaryStore;
  private cursor: string | undefined;
  private noticeCursor: string | undefined;
  private operationCursor: string | undefined;
  readonly recoveryIssues = new Map<string, string>();
  private readonly epoch: Promise<string>;
  constructor(readonly client: RpcClient, readonly settings: Settings, readonly owner: { serviceNodeId: string; generation: number }, readonly signal: AbortSignal, readonly now: () => Date = () => new Date(), dataRoot?: string) {
    validate('Settings', settings); checkPolicy(settings.policy);
    need(new Set(settings.sources.map(source => source.sourceId)).size === settings.sources.length, 'invalid_arguments', 'Each Secretary source ID must be unique.');
    this.store = new SecretaryStore(client, settings.identity.scope.rootObjectId, dataRoot);
    this.epoch = runtimeEpoch(client);
  }
  sourceDefinition(id: string): SourceDefinition {
    const source = this.settings.sources.find(value => value.sourceId === id); need(source, 'secretary_source_unknown', 'The selected source is not configured.'); return source;
  }
  authorize(caller: string, scope: Scope, action: Request['action'] | 'operation' | 'status', sourceId?: string): void {
    need(same(scope, this.settings.identity.scope), 'secretary_scope_conflict', 'Select this exact Secretary scope.');
    if (action === 'capture' || action === 'checkpoint' || action === 'attach') need(sourceId && this.sourceDefinition(sourceId).producerPrincipalIds.includes(caller), 'forbidden', 'Only this source’s admitted producer may capture or advance it.');
  }
  async verifyOwner(): Promise<void> {
    this.signal.throwIfAborted();
    const owner = await connectionOwner(this.client,this.owner.serviceNodeId);
    need(owner.connected && owner.synced && owner.principalId === this.settings.identity.principalId && owner.hostId === this.settings.identity.hostId && owner.serviceName === 'secretary', 'secretary_owner_mismatch', 'Secretary work requires its original authenticated live owner.');
  }
  async initialize(): Promise<void> {
    await this.verifyOwner(); const root = await this.client.request('objects.stat', { objectId: this.store.root }); need(!root.effectivelyArchived, 'secretary_scope_conflict', 'The Secretary root is archived.'); this.store.cleanup(this.now().getTime());
    for (const definition of this.settings.sources) {
      const value = { schemaVersion: 1 as const, definition, definitionHash: hashJson(definition), cursor: null, checkpointOperation: null, updatedAt: null };
      await this.store.create('secretary/source', sourceName(definition.sourceId), value, mutation(this.store.root, 'source', sourceIdentity(definition)));
      const source = await this.source(definition.sourceId);
      if (!same(source.value.definition, definition)) {
        await this.verifyOwner();
        await this.store.amend(source, 'secretary/source', { ...source.value, definition, definitionHash: hashJson(definition) });
      }
    }
  }
  async source(id: string): Promise<Document<'secretary/source'>> {
    const source = await this.store.named('secretary/source', sourceName(id)); need(source, 'secretary_evidence_missing', 'The original source checkpoint is missing.');
    const definition = this.settings.sources.find(value => value.sourceId === id);
    need(source.value.definition.sourceId === id && source.value.definitionHash === hashJson(source.value.definition) && (!definition || same(sourceIdentity(definition), sourceIdentity(source.value.definition))),
      'secretary_identity_conflict', 'A source checkpoint cannot change its original source/account definition.');
    return source;
  }
  async item(selected: Pin | string): Promise<Document<'secretary/item'>> {
    const item = await this.store.read('secretary/item', selected), value = item.value;
    const source = (await this.source(value.sourceId)).value.definition;
    need(value.identityHash === identityHash(value.sourceId, value.message) && value.messageHash === messageHash(value.message) && value.message.accountId === source.accountId,
      'secretary_evidence_conflict', 'The item must retain its original source, message and content identity.');
    if (value.decision) {
      need(value.notice && value.notice.text === noticeText(value.decision.assessment, value.message, source) && value.notice.source.objectId === item.pin.objectId,
        'secretary_evidence_conflict', 'The assessment and original notification source cannot change.');
    } else need(!value.notice, 'secretary_evidence_conflict', 'An unassessed item has no notice.');
    return item;
  }
  async checked(operation: Document<'secretary/operation'>): Promise<Document<'secretary/operation'>> {
    const original = await this.store.original('secretary/operation', operation), value = operation.value, first = original.value;
    need(first.phase === 'accepted' && first.effect === null && first.errorCode === null && same(immutable(first), immutable(value)) &&
      (!value.request ? value.action === 'capture' && value.ignoredReason && !value.prepared : value.request.action === value.action && value.request.operationId === value.operationId && value.requestHash === hashJson(value.request) && same(value.request.expectedScope, this.settings.identity.scope)),
      'secretary_evidence_conflict', 'The original accepted action and prepared bytes cannot be rewritten.');
    const originalSource = await this.source(value.sourceId);
    need(value.source.definitionHash === hashJson(value.source.definition) && same(sourceIdentity(value.source.definition), sourceIdentity(originalSource.value.definition)),
      'secretary_evidence_conflict', 'The accepted source configuration must retain its original identity.');
    return operation;
  }
  async find(caller: string, id: string): Promise<Document<'secretary/operation'> | null> {
    const result = await this.store.named('secretary/operation', operationName(caller, id));
    if (!result) return null;
    need(result.value.callerPrincipalId === caller && result.value.operationId === id, 'secretary_identity_conflict', 'This operation belongs to another caller or ID.'); return this.checked(result);
  }
  async operation(caller: string, scope: Scope, id: string): Promise<Accepted> {
    this.authorize(caller, scope, 'operation'); await this.verifyOwner(); const value = await this.find(caller, id); need(value, 'not_found', 'The caller has no retained operation with this ID.'); return value.value;
  }
  private async workflowReceipt(value: WorkflowOperation): Promise<Pin | null> {
    const { mutationId, ...arguments_ } = value.write;
    need(mutationId === deriveOperationId(value.operationId, ['secretary-workflow', value.callerPrincipalId]),
      'secretary_evidence_conflict', 'The saved write has a different operation identity.');
    const result = await this.client.request('objects.writeReceipt', { mutationId, expectedRequestHash: hashJson({ method: 'objects.write', arguments: arguments_ }) });
    if (!result) return null;
    const expectedKey = value.action === 'configure_execution' ? 'secretary/configuration' : 'secretary/assignment';
    need(result.object.contractKey === expectedKey && result.object.parentId === this.store.root &&
      (!value.write.objectId || result.object.id === value.write.objectId) &&
      (!value.write.create || result.object.name === value.write.create.name),
      'secretary_evidence_conflict', 'The original write receipt names another Secretary object.');
    return { objectId: result.object.id, revision: result.revision.revision };
  }
  async operationRead(caller: string, scope: Scope, id: string): Promise<OperationOutcome> {
    this.authorize(caller, scope, 'operation'); await this.verifyOwner();
    const inbox = await this.find(caller, id), observedAt = this.now().toISOString();
    if (inbox) return { operationId: id, action: inbox.value.action, phase: inbox.value.phase, effect: inbox.value.effect,
      errorCode: inbox.value.errorCode, observedAt };
    const row = this.store.technicalNamed<WorkflowOperation>(workflowOperationKey, operationName(caller, id));
    need(row && row.value.callerPrincipalId === caller && row.value.operationId === id,
      'not_found', 'The caller has no retained operation with this ID.');
    const effect = await this.workflowReceipt(row.value);
    return { operationId: id, action: row.value.action, phase: effect ? 'succeeded' : row.value.phase === 'failed' ? 'failed' : 'unknown',
      effect, errorCode: effect ? null : row.value.errorCode, observedAt };
  }
  private async saveWorkflow(caller: string, request: SaveAssignmentRequest | SaveConfigurationRequest,
    action: WorkflowOperation['action'], write: Wire.ObjectWrite | null): Promise<Pin> {
    const name = operationName(caller, request.operationId), requestHash = hashJson(request);
    need(!(await this.find(caller, request.operationId)), 'mutation_conflict', 'The operation identity already names an inbox action.');
    let row = this.store.technicalNamed<WorkflowOperation>(workflowOperationKey, name);
    const retry = !!row;
    if (!row) {
      need(write, 'secretary_evidence_missing', 'The original Secretary write was not prepared.');
      row = this.store.technicalCreate(workflowOperationKey, name, { operationId: request.operationId, callerPrincipalId: caller,
        action, requestHash, write, phase: 'prepared', errorCode: null, expiresAt: Date.now() + 24 * 60 * 60 * 1000 });
    }
    need(row.value.callerPrincipalId === caller && row.value.operationId === request.operationId && row.value.action === action && row.value.requestHash === requestHash,
      'mutation_conflict', 'The operation identity already names different Secretary arguments.');
    const receipt = retry ? await this.workflowReceipt(row.value) : null;
    if (receipt) return receipt;
    need(row.value.phase !== 'failed', row.value.errorCode ?? 'secretary_operation_failed', 'The original Secretary write failed.');
    try { return await this.store.writePrepared(row.value.write); }
    catch (error) {
      const failure = IvyError.from(error);
      if (!retry && failure.outcome === 'not_executed') {
        const latest = this.store.technicalNamed<WorkflowOperation>(workflowOperationKey, name);
        if (latest && latest.value.phase === 'prepared')
          this.store.technicalAmend(workflowOperationKey, latest, { ...latest.value, phase: 'failed', errorCode: failure.code });
      }
      throw error;
    }
  }
  private validateMessage(message: Message, source: SourceDefinition): void {
    need(message.accountId === source.accountId, 'secretary_account_mismatch', 'The observation must belong to the selected source account.');
    for (const stamp of [message.observedAt, message.occurredAt]) need(Number.isFinite(Date.parse(stamp)) && new Date(stamp).toISOString() === stamp, 'invalid_arguments', 'Observation times must be real UTC instants.');
    if (message.url) {
      let url: URL; try { url = new URL(message.url); } catch { throw new IvyError('invalid_arguments', 'Source links must be absolute HTTP(S) URLs.'); }
      need(['https:', 'http:'].includes(url.protocol) && !url.username && !url.password, 'invalid_arguments', 'Source links must be absolute HTTP(S) URLs without credentials.');
    }
  }
  private async validateAssessment(assessment: Assessment, policy: Policy): Promise<void> {
    need((assessment.disposition === 'task') === (assessment.task !== null), 'invalid_arguments', 'Only task disposition must identify an already saved TaskBoard task.');
    need((['ignore', 'record'].includes(assessment.disposition)) === (assessment.notification === 'none'), 'invalid_arguments', 'Ignored or recorded items must use no notification; actionable items must select Main, Voice or needs-attention.');
    need(assessment.notification !== 'voice' || assessment.urgency === 'critical' && policy.voiceEscalation, 'invalid_arguments', 'Voice requires one available escalation target and a critical assessment.');
    if (!assessment.task) return;
    const task = await this.client.request('objects.read', assessment.task);
    need(!task.object.effectivelyArchived && task.object.contractKey === 'task-board/task' && task.revision.contractVersion === '1.0.0' && task.content.encoding === 'json' && hashJson(task.content.value) === task.revision.contentHash,
      'secretary_task_mismatch', 'The linked delegation must be an exact active saved TaskBoard task.');
  }
  private async validateMedia(item: Document<'secretary/item'>, media: ItemMedia[]): Promise<void> {
    need(media.length > 0 && media.length <= 64 && new Set(media.map(value => hashJson(value))).size === media.length, 'invalid_arguments', 'Media attachments must be distinct and bounded.');
    for (const entry of media) {
      need((entry.pin !== null) === (entry.contentHash !== null) && (entry.pin === null) === (entry.errorCode !== null), 'invalid_arguments', 'Media must contain either one exact artifact or one explicit gap.');
      if (!entry.pin) continue;
      const raw = await this.client.request('objects.read', entry.pin);
      need(raw.object.contractKey === entry.contractKey && raw.object.ownerObjectId === item.pin.objectId && !raw.object.effectivelyArchived && raw.revision.contentHash === entry.contentHash,
        'secretary_media_mismatch', 'The media artifact must be exact final data owned by this Secretary item.');
    }
  }
  async action(caller: string, request: Request): Promise<Accepted> {
    validate('Request', request); this.authorize(caller, request.expectedScope, request.action, 'sourceId' in request ? request.sourceId : undefined);
    need(!this.store.technicalNamed<WorkflowOperation>(workflowOperationKey, operationName(caller, request.operationId)),
      'mutation_conflict', 'The operation identity already names an assignment action.');
    const identity = parseOperationId(request.operationId), now = Date.now();
    need(identity.runtimeEpoch === await this.epoch && identity.issuedAtUnixMs <= now + 60_000 && identity.issuedAtUnixMs >= this.store.expiredBefore() && now < identity.issuedAtUnixMs + 24 * 60 * 60 * 1000,
      'operation_expired', 'The Secretary operation belongs to another runtime or is outside its replay window.');
    if (request.action === 'capture') this.validateMessage(request.message, this.sourceDefinition(request.sourceId));
    await this.verifyOwner(); const prior = await this.find(caller, request.operationId);
    if (prior) { need(prior.value.requestHash === hashJson(request), 'mutation_conflict', 'This operation ID already names different arguments.'); return (await this.apply(prior)).value; }
    const at = this.now().toISOString();
    const sourceId = 'sourceId' in request ? request.sourceId : (await this.item(request.item.objectId)).value.sourceId;
    const sourceSnapshot = await this.source(sourceId);
    const accepted: Accepted = { schemaVersion: 1, action: request.action, operationId: request.operationId, callerPrincipalId: caller, requestHash: hashJson(request), request: structuredClone(request), sourceId: '', createdAt: at,
      source: structuredClone(sourceSnapshot.value), prepared: null, existingEffect: null, ignoredReason: null, captureIdentity: null, checkpointCaptures: [], phase: 'accepted', effect: null, errorCode: null };
    if (request.action === 'capture') {
      accepted.sourceId = request.sourceId; const source = this.sourceDefinition(request.sourceId); accepted.ignoredReason = ignored(source, request.message);
      if (accepted.ignoredReason) accepted.request = null;
      else {
        accepted.captureIdentity = identityHash(request.sourceId, request.message);
        const item: Item = { schemaVersion: 1, identityHash: accepted.captureIdentity, sourceId: request.sourceId, messageHash: messageHash(request.message), message: structuredClone(request.message), media: [], decision: null, notice: null };
        const name = 'item-' + item.identityHash.slice(7), existing = await this.store.named('secretary/item', name);
        if (existing) accepted.existingEffect = await this.captureDuplicate(existing, item);
        else accepted.prepared = { key: 'secretary/item', destination: { createName: name }, value: item };
      }
    } else if (request.action === 'assess') {
      const current = await this.item(request.item.objectId);
      need(same(current.pin, request.item) && current.value.decision === null, 'revision_conflict', 'Assess the exact current unassessed item once.');
      const policy: Policy = { ...this.settings.policy, voiceEscalation: this.settings.policy.voiceEscalation ??
        (request.assessment.notification === 'voice' ? await discoverVoiceTarget(this.client) : null) };
      await this.validateAssessment(request.assessment, policy); accepted.sourceId = current.value.sourceId;
      const reason = suppression(this.sourceDefinition(accepted.sourceId), request.assessment, policy);
      accepted.prepared = { key: 'secretary/item', destination: { object: current.pin }, value: { ...current.value,
        decision: { assessment: request.assessment, actor: caller, operationId: request.operationId, decidedAt: at, policy: structuredClone(policy) },
        notice: { state: reason ? 'suppressed' : 'queued', reason, text: noticeText(request.assessment, current.value.message, this.sourceDefinition(current.value.sourceId)), source: { objectId: current.pin.objectId, revision: current.pin.revision + 1 }, target: null, request: null, preparedRevision: null, evidence: null,
          voice: request.assessment.notification === 'voice' ? { state: 'queued', operationId: randomUUID(), callId: null, errorCode: null } : null, updatedAt: at } } };
    } else if (request.action === 'attach') {
      accepted.sourceId = request.sourceId;
      const current = await this.item(request.item.objectId);
      need(same(current.pin, request.item) && current.value.sourceId === request.sourceId, 'revision_conflict', 'Attach media to the exact current source item.');
      await this.validateMedia(current, request.media);
      const media = [...current.value.media];
      for (const entry of request.media) if (!media.some(value => same(value, entry))) media.push(structuredClone(entry));
      need(media.length <= 64, 'limit_exceeded', 'The Secretary item media list is full.');
      accepted.prepared = { key: 'secretary/item', destination: { object: current.pin }, value: { ...current.value, media } };
    } else {
      accepted.sourceId = request.sourceId;
      need(request.nextCursor !== request.previousCursor && new Set(request.captures).size === request.captures.length, 'invalid_arguments', 'A checkpoint must advance with distinct capture operations.');
      for (const id of request.captures) {
        const capture = await this.find(caller, id);
        need(capture?.value.phase === 'succeeded' && capture.value.action === 'capture' && capture.value.sourceId === request.sourceId, 'secretary_capture_pending', 'Every checkpoint member must be this producer’s completed capture from this source.');
        accepted.checkpointCaptures.push(capture.pin);
      }
      const source = await this.source(request.sourceId);
      need(source.value.cursor === request.previousCursor, 'revision_conflict', 'The source has a different current checkpoint.');
      accepted.prepared = { key: 'secretary/source', destination: { object: source.pin }, value: { ...source.value, cursor: request.nextCursor, checkpointOperation: { principalId: caller, operationId: request.operationId }, updatedAt: at } };
    }
    const operation = await this.store.create('secretary/operation', operationName(caller, request.operationId), accepted, mutation(caller, request.operationId, 'accept'));
    need(operation.value.requestHash === accepted.requestHash, 'mutation_conflict', 'The winning operation names different original arguments.');
    return (await this.apply(await this.checked(operation))).value;
  }
  private async captureDuplicate(existing: Document<'secretary/item'>, proposed: Item): Promise<Pin> {
    const item = await this.item(existing.pin.objectId);
    need(item.value.sourceId === proposed.sourceId && item.value.identityHash === proposed.identityHash && item.value.messageHash === proposed.messageHash,
      'secretary_identity_conflict', 'The same native message revision has different original content.'); return item.pin;
  }
  async apply(operation: Document<'secretary/operation'>): Promise<Document<'secretary/operation'>> {
    await this.verifyOwner(); operation = await this.checked(await this.store.read('secretary/operation', operation.pin.objectId));
    if (operation.value.phase !== 'accepted') return operation;
    const value = operation.value, prepared = value.prepared; let effect = value.existingEffect;
    // Reconcile an already committed effect before checking permission for a new write.
    if (prepared) {
      let retained: Document<'secretary/item' | 'secretary/source'> | null = null;
      try {
        retained = 'createName' in prepared.destination ? await this.store.named(prepared.key, prepared.destination.createName)
          : await this.store.read(prepared.key, { objectId: prepared.destination.object.objectId, revision: prepared.destination.object.revision + 1 });
      } catch (error) { if (!(error instanceof IvyError && error.code === 'not_found')) throw error; }
      if (retained && prepared.key === 'secretary/item' && 'createName' in prepared.destination) effect = await this.captureDuplicate(retained as Document<'secretary/item'>, prepared.value);
      else if (retained && same(retained.value, prepared.value)) effect = retained.pin;
      if (effect) return this.finish(operation, { ...value, phase: 'succeeded', effect, errorCode: null });
      const source = this.settings.sources.find(source => source.sourceId === value.sourceId);
      need(source && (value.action === 'assess' || source.producerPrincipalIds.includes(value.callerPrincipalId)), 'secretary_authorization_changed', 'Original pending producer work is retained until its source identity is currently authorized.');
      if (prepared.key === 'secretary/item') need(!ignored(source, prepared.value.message), 'secretary_authorization_changed', 'The current source policy no longer admits a new effect for this message.');
    }
    try {
      if (prepared) {
        try { effect = await this.store.write(prepared.key, prepared.value, prepared.destination, mutation(value.callerPrincipalId, value.operationId, 'effect')); }
        catch (error) {
          if (value.action !== 'capture' || prepared.key !== 'secretary/item' || !('createName' in prepared.destination)) throw error;
          const duplicate = await this.store.named('secretary/item', prepared.destination.createName); if (!duplicate) throw error;
          effect = await this.captureDuplicate(duplicate, prepared.value);
        }
      }
      return await this.finish(operation, { ...value, phase: 'succeeded', effect, errorCode: null });
    } catch (error) {
      if (!conflict(error) && !(error instanceof IvyError && error.code === 'secretary_identity_conflict')) throw error;
      return this.finish(operation, { ...value, phase: 'failed', effect: null, errorCode: IvyError.from(error).code });
    }
  }
  private async finish(operation: Document<'secretary/operation'>, next: Accepted): Promise<Document<'secretary/operation'>> {
    try { return await this.store.amend(operation, 'secretary/operation', next); }
    catch (error) {
      const saved = await this.checked(await this.store.read('secretary/operation', operation.pin.objectId));
      if (saved.value.phase !== 'accepted') { need(saved.value.phase === next.phase && same(saved.value.effect, next.effect), 'secretary_evidence_conflict', 'The retained action outcome differs from its original effect.'); return saved; }
      throw error;
    }
  }
  async tick(): Promise<void> {
    await this.verifyOwner();
    const ids = this.store.acceptedIds(this.settings.recordsPerTick, this.operationCursor);
    this.operationCursor = ids.length < this.settings.recordsPerTick ? undefined : ids.at(-1);
    for (const id of ids) {
      try { await this.apply(await this.store.read('secretary/operation', id)); this.recoveryIssues.delete(id); }
      catch (error) { if (this.signal.aborted) throw error; this.issue(id, IvyError.from(error).code); }
    }
  }
  issue(objectId: string, code: string): void {
    if (this.recoveryIssues.has(objectId) || this.recoveryIssues.size < 31) this.recoveryIssues.set(objectId, code);
    else this.recoveryIssues.set('recovery-overflow', 'secretary_recovery_issues_overflow');
  }
  async noticePage(): Promise<string[]> {
    const dispatching = ['queued', 'dispatching'].map(state => ({ op: 'eq' as const, field: 'data:/notice/state', value: state }));
    const voice = ['queued', 'outcome_unknown'].map(state => ({ op: 'eq' as const, field: 'data:/notice/voice/state', value: state }));
    const page = await this.client.request('objects.query', { contractKey: 'secretary/item', limit: this.settings.recordsPerTick, ...(this.noticeCursor ? { cursor: this.noticeCursor } : {}),
      orderBy: [{ field: 'object.id', direction: 'asc' }], where: { op: 'and', args: [{ op: 'eq', field: 'object.parentId', value: this.store.root },
        { op: 'or', args: [...dispatching, { op: 'and' as const, args: [{ op: 'eq' as const, field: 'data:/notice/state', value: 'confirmed' }, { op: 'or' as const, args: voice }] }] } ] } });
    this.noticeCursor = page.nextCursor ?? undefined; return page.items.map(row => row.objectId);
  }
  async status(caller: string, scope: Scope): Promise<Status> {
    this.authorize(caller, scope, 'status'); await this.verifyOwner(); const sources = [];
    for (const definition of this.settings.sources) { const source = await this.source(definition.sourceId); sources.push({ object: source.pin, data: source.value }); }
    const policy = { ...this.settings.policy, voiceEscalation: this.settings.policy.voiceEscalation ?? await discoverVoiceTarget(this.client) };
    let noticeConfigured = false;
    try { noticeConfigured = await this.noticeTarget() !== null; } catch (error) { if (!(error instanceof IvyError)) throw error; }
    return { identity: this.settings.identity, sources, policy, noticeConfigured, recoveryIssues: [...this.recoveryIssues].map(([objectId, code]) => ({ objectId, code })), observedAt: this.now().toISOString() };
  }
  async listAssignments(
    caller: string,
    request: ListAssignmentsRequest,
  ): Promise<AssignmentPage> {
    validate('ListAssignmentsRequest', request);
    this.authorize(caller, request.expectedScope, 'status');
    await this.verifyOwner();
    const where: Wire.QueryPredicate[] = [{ op: 'eq', field: 'object.parentId', value: this.store.root }];
    if (request.assignmentId) where.push({ op: 'eq', field: 'data:/assignmentId', value: request.assignmentId });
    if (request.enabled !== undefined) where.push({ op: 'eq', field: 'data:/enabled', value: request.enabled });
    if (request.triggerKind) where.push({ op: 'eq', field: 'data:/trigger/kind', value: request.triggerKind });
    const page = await this.client.request('objects.query', {
      contractKey: 'secretary/assignment',
      includeContent: true,
      limit: request.limit ?? 50,
      ...(request.cursor ? { cursor: request.cursor } : {}),
      orderBy: [{ field: 'object.id', direction: 'asc' }],
      where: { op: 'and', args: where },
    });
    const assignments = page.items.map((row) => {
      const current = this.store.decode(
        'secretary/assignment',
        queryDocument(row),
      );
      return { pin: current.pin, assignment: current.value };
    });
    return { assignments, nextCursor: page.nextCursor ?? null, observedAt: this.now().toISOString() };
  }
  async getAssignment(caller: string, request: GetAssignmentRequest): Promise<AssignmentEntryResponse> {
    validate('GetAssignmentRequest', request); this.authorize(caller, request.expectedScope, 'status'); await this.verifyOwner();
    const current = await this.store.named('secretary/assignment', assignmentName(request.assignmentId));
    need(current && current.value.assignmentId === request.assignmentId, 'not_found', 'The Secretary assignment was not found.');
    return { pin: current.pin, assignment: current.value, observedAt: this.now().toISOString() };
  }
  async saveConfiguration(caller: string, request: SaveConfigurationRequest): Promise<Pin> {
    validate('SaveConfigurationRequest', request); validateConfiguration(request.value); await this.verifyOwner();
    const prior = this.store.technicalNamed<WorkflowOperation>(workflowOperationKey, operationName(caller, request.operationId));
    if (prior) return this.saveWorkflow(caller, request, 'configure_execution', null);
    const current = await this.store.named('secretary/configuration', 'configuration');
    need(current && same(current.pin, request.configuration), 'revision_conflict', 'Reload the current Secretary configuration before saving.');
    const value = { ...structuredClone(request.value), rules: unifiedMainRules(request.value.rules), updatedAt: this.now().toISOString() };
    return await this.saveWorkflow(caller, request, 'configure_execution', this.store.objectWriteRequest('secretary/configuration', value, { object: current.pin },
      deriveOperationId(request.operationId, ['secretary-workflow', caller])));
  }
  async saveAssignment(caller: string, request: SaveAssignmentRequest): Promise<Pin> {
    validate('SaveAssignmentRequest', request); validateAssignment(request.value); await this.verifyOwner();
    const action = request.assignment ? 'update_assignment' : 'create_assignment';
    const prior = this.store.technicalNamed<WorkflowOperation>(workflowOperationKey, operationName(caller, request.operationId));
    if (prior) return this.saveWorkflow(caller, request, action, null);
    const name = assignmentName(request.value.assignmentId), current = await this.store.named('secretary/assignment', name), now = this.now().toISOString();
    if (!request.assignment) {
      need(!current, 'secretary_identity_conflict', 'A new assignment needs a fresh identity.');
      const value = { ...structuredClone(request.value), rules: unifiedRuleOverrides(request.value.rules), createdAt: now, updatedAt: now };
      return await this.saveWorkflow(caller, request, action, this.store.objectWriteRequest('secretary/assignment', value, { createName: name },
        deriveOperationId(request.operationId, ['secretary-workflow', caller])));
    }
    need(current && same(current.pin, request.assignment), 'revision_conflict', 'Reload the current Secretary assignment before saving.');
    need(current.value.assignmentId === request.value.assignmentId && current.value.createdAt === request.value.createdAt,
      'secretary_identity_conflict', 'An assignment update cannot replace its retained identity.');
    const value = { ...structuredClone(request.value), rules: unifiedRuleOverrides(request.value.rules), updatedAt: now };
    return await this.saveWorkflow(caller, request, action, this.store.objectWriteRequest('secretary/assignment', value, { object: current.pin },
      deriveOperationId(request.operationId, ['secretary-workflow', caller])));
  }
}
