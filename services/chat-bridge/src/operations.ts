import { canonical, hashJson } from '../../../packages/sdk/src/node.js';
import { IvyError, requireThat } from '../../../packages/sdk/src/node.js';
import type { Chat } from '../../../packages/sdk/src/node.js';
import { validateChat } from '../../../packages/sdk/src/node.js';
import { ChatAdmission } from './admission.js';
import { ChatStore, mutation } from './store.js';
import type { Document } from './store.js';

export const contention = (error: unknown) => error instanceof IvyError && ['revision_conflict', 'mutation_conflict'].includes(error.code);
type OperationDocument = Document<'chat-bridge/operation'>;
export class ChatOperations {
  constructor(readonly store: ChatStore, readonly admission: ChatAdmission) {
    requireThat(store.rootObjectId === admission.definition.rootObjectId, 'chat_scope_mismatch', 'The store must use the configured ChatBridge root.');
  }
  name(caller: string, operationId: string): string {
    return 'Chat operation ' + hashJson({ principalId: this.admission.definition.principalId, workspaceId: this.admission.definition.workspaceId, caller, operationId }).slice(7);
  }
  check(value: Chat.Operation): void {
    const expected = this.admission.expected(value.callerPrincipalId, value.request.action === 'notify' ? 'notice' : value.request.action === 'createMain' ? 'either' : 'reader');
    const original = value.request.expectedBridge;
    requireThat(value.workspaceId === expected.workspaceId && value.definitionHash === original.definitionHash && value.request.operationId === value.operationId &&
      hashJson(value.request) === value.requestHash && original.principalId === expected.principalId &&
      original.callerPrincipalId === expected.callerPrincipalId && original.workspaceId === expected.workspaceId,
      'chat_identity_conflict', 'The saved operation must retain its original Main, caller and request.');
    requireThat(!value.outcome || value.outcome.operationId === value.operationId && value.outcome.action === value.request.action, 'chat_identity_conflict', 'The saved outcome belongs to another operation or action.');
  }
  async find(caller: string, operationId: string): Promise<OperationDocument | null> {
    const result = await this.store.named('chat-bridge/operation', this.name(caller, operationId));
    if (result) { this.check(result.value); requireThat(result.value.callerPrincipalId === caller && result.value.operationId === operationId, 'chat_identity_conflict', 'The durable operation name has another caller or identity.'); }
    return result;
  }
  async begin(caller: string, original: Chat.AdmittedActionRequest): Promise<OperationDocument> {
    this.admission.authorize(caller, original.expectedBridge, 'channel' in original ? original.channel : undefined,
      original.action === 'notify' ? 'notice' : original.action === 'createMain' ? 'either' : 'reader');
    validateChat('AdmittedActionRequest', original);
    const request = structuredClone(original), requestHash = hashJson(request), existing = await this.find(caller, request.operationId);
    if (existing) { requireThat(existing.value.requestHash === requestHash, 'mutation_conflict', 'This operation already names different ChatBridge arguments.'); return existing; }
    const at = new Date().toISOString(), value: Chat.Operation = { schemaVersion: 1, workspaceId: this.admission.definition.workspaceId,
      definitionHash: request.expectedBridge.definitionHash, callerPrincipalId: caller, operationId: request.operationId, request, requestHash,
      phase: 'accepted', nativeCall: null, outcome: null, error: null, createdAt: at, updatedAt: at };
    try {
      const name = this.name(caller, request.operationId), pin = await this.store.write('chat-bridge/operation', value, mutation(name, 'accept'), { create: { parentId: this.store.rootObjectId, name } });
      return this.store.read('chat-bridge/operation', pin);
    } catch (error) {
      if (!contention(error)) throw error;
      const winner = await this.find(caller, request.operationId);
      requireThat(winner && winner.value.requestHash === requestHash, 'mutation_conflict', 'The concurrent action has different original arguments.'); return winner;
    }
  }
  async finish(id: string, outcome: Chat.ActionOutcome): Promise<OperationDocument> {
    validateChat('ActionOutcome', outcome);
    for (let attempt = 0; attempt < 16; attempt++) {
      const current = await this.store.read('chat-bridge/operation', id); this.check(current.value);
      requireThat(current.value.operationId === outcome.operationId && current.value.request.action === outcome.action, 'chat_identity_conflict', 'An action receipt must preserve its original identity.');
      if (current.value.phase === 'succeeded') { requireThat(canonical(current.value.outcome) === canonical(outcome), 'mutation_conflict', 'The original action has another saved outcome.'); return current; }
      requireThat(current.value.phase !== 'failed', 'chat_operation_failed', 'A failed action cannot be completed by a new receipt.');
      const next: Chat.Operation = { ...current.value, phase: 'succeeded', outcome: structuredClone(outcome), error: null, updatedAt: new Date().toISOString() };
      try {
        const pin = await this.store.write('chat-bridge/operation', next, mutation(id, 'finish:' + current.pin.revision + ':' + hashJson(next)), { objectId: id, expectedRevision: current.pin.revision });
        return this.store.read('chat-bridge/operation', pin);
      } catch (error) { if (!contention(error)) throw error; }
    }
    throw new IvyError('chat_contention', 'The action changed repeatedly. Recover its original operation.', 'unknown');
  }
  /** Call only after proving that the original admission has not applied its Main/Input change. */
  async rejectUnapplied(id: string, code: string, message: string): Promise<OperationDocument> {
    for (let attempt = 0; attempt < 16; attempt++) {
      const current = await this.store.read('chat-bridge/operation', id); this.check(current.value);
      if (current.value.phase === 'failed') return current;
      requireThat(current.value.phase === 'accepted' && current.value.nativeCall === null, 'chat_operation_unresolved', 'Only an unexecuted admission may be refused.');
      const next: Chat.Operation = { ...current.value, phase: 'failed', error: { code, detail: message, outcome: 'not_executed' }, updatedAt: current.value.createdAt };
      try {
        const pin = await this.store.write('chat-bridge/operation', next, mutation(id, 'refuse:' + current.pin.revision + ':' + hashJson(next)), { objectId: id, expectedRevision: current.pin.revision });
        return this.store.read('chat-bridge/operation', pin);
      } catch (error) { if (!contention(error)) throw error; }
    }
    throw new IvyError('chat_contention', 'The refusal needs original-operation recovery.', 'unknown');
  }
}
