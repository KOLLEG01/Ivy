import { canonical, hashJson } from '../../../packages/sdk/src/node.js';
import { IvyError, requireThat } from '../../../packages/sdk/src/node.js';
import type { Chat } from '../../../packages/sdk/src/node.js';
import { ChatOperations, contention } from './operations.js';
import { mutation } from './store.js';
import type { Document } from './store.js';

type MainDocument = Document<'chat-bridge/main'>;
const definitionIdentity = (definition: Chat.SavedDefinition) => ({
  principalId: definition.principalId, workspaceId: definition.workspaceId, definitionHash: hashJson(definition),
});
// Immutable Chat definitions own separate durable identities. A configuration
// change must not make startup depend on, overwrite, or silently reinterpret a
// Main, conversation or input created under the previous definition.
export const mainName = (definition: Chat.SavedDefinition) => 'Chat Main ' + hashJson(definitionIdentity(definition)).slice(7);
export const conversationName = (definition: Chat.SavedDefinition) => 'Chat conversation ' + hashJson(definitionIdentity(definition)).slice(7);
export const inputName = (definition: Chat.SavedDefinition, identity: Chat.InputIdentity, definitionHash = hashJson(definition)) =>
  'Chat input ' + hashJson({ principalId: definition.principalId, workspaceId: definition.workspaceId, definitionHash, identity }).slice(7);
const same = (a: unknown, b: unknown) => canonical(a) === canonical(b);

/** Durable input admission only. A separate native worker may act on the saved queue head. */
export class ChatQueue {
  constructor(readonly operations: ChatOperations, readonly ensureBinding?: (caller: string, operationId: string) => Promise<Chat.BindingView>) {}
  get store() { return this.operations.store; }
  get admission() { return this.operations.admission; }
  async main(): Promise<MainDocument> {
    const main = await this.store.named('chat-bridge/main', mainName(this.admission.definition));
    requireThat(main, 'chat_main_missing', 'No Main aggregate exists for this ChatBridge workspace.');
    return this.checkMain(main);
  }
  checkMain(main: MainDocument): MainDocument {
    requireThat(main.value.definitionHash === this.admission.definitionHash && same(main.value.definition, this.admission.definition),
      'chat_definition_mismatch', 'The stored Main belongs to a different ChatBridge configuration.');
    const queue = main.value.queue;
    requireThat(new Set(queue.map(ticket => ticket.inputId)).size === queue.length && new Set(queue.map(ticket => hashJson(ticket.identity))).size === queue.length &&
      queue.every((ticket, index) => ticket.sequence < main.value.nextSequence && (index === 0 || ticket.sequence > queue[index - 1]!.sequence)),
      'chat_identity_conflict', 'The saved Main queue contains duplicate or out-of-order identities.');
    const publication = main.value.publication;
    if (publication) {
      const end = publication.firstSequence + publication.partCount;
      requireThat(queue[0]?.inputId === publication.inputId && publication.publishedParts <= publication.partCount && Number.isSafeInteger(end) &&
        end <= main.value.nextSequence && queue.every(ticket => ticket.sequence < publication.firstSequence || ticket.sequence >= end),
        'chat_identity_conflict', 'Result publication must reserve its complete sequence range for the current queue head.');
    }
    return main;
  }
  private async originalInput(identity: Chat.InputIdentity, requestHash: string): Promise<Document<'chat-bridge/input'> | null> {
    const input = await this.store.named('chat-bridge/input', inputName(this.admission.definition, identity));
    if (!input) return null;
    requireThat(input.value.workspaceId === this.admission.definition.workspaceId && input.value.definitionHash === this.admission.definitionHash &&
      same(input.value.identity, identity) && input.value.requestHash === requestHash,
      'mutation_conflict', 'The original message identity already names different input or a different Main binding.');
    return input;
  }
  private async binding(pin: Chat.ObjectPin): Promise<void> {
    const binding = (await this.store.read('chat-bridge/binding', pin)).value, definition = this.admission.definition;
    requireThat(binding.workspaceId === definition.workspaceId && binding.definitionHash === this.admission.definitionHash && same(binding.project, definition.project) &&
      same(binding.nativePlan, definition.nativePlan) && binding.primary.serviceNodeId === definition.project.serviceNodeId && binding.primary.namespace === 'codex' && binding.primary.kind === 'thread',
      'chat_binding_mismatch', 'Main must retain the exact configured native project, plan and owning thread.');
  }
  private async selectedBinding(caller: string, request: Chat.SendRequest): Promise<Chat.ObjectPin> {
    if (request.expectedBinding) return request.expectedBinding;
    requireThat(this.ensureBinding, 'chat_main_missing', 'This message ingress cannot create its first Main.');
    return (await this.ensureBinding(caller, request.operationId)).object;
  }
  private async writeMain(current: MainDocument, value: Chat.Main, scope: string, step: string): Promise<MainDocument> {
    const pin = await this.store.write('chat-bridge/main', value, mutation(scope, step + ':' + current.pin.revision + ':' + hashJson(value)),
      { objectId: current.pin.objectId, expectedRevision: current.pin.revision });
    return this.store.read('chat-bridge/main', pin);
  }
  /** Resume the exact pending send, including a committed write whose response was lost. */
  async recoverPending(): Promise<void> {
    for (let attempt = 0; attempt < 32; attempt++) {
      let main = await this.main();
      if (!main.value.pendingAction) return;
      const accepted = await this.store.read('chat-bridge/operation', main.value.pendingAction); this.operations.check(accepted.value);
      requireThat(accepted.value.request.action === 'send', 'chat_pending_action', 'Another Main action needs its own recovery before new input.');
      const request = accepted.value.request, admitted = this.admission.send(accepted.value.callerPrincipalId, request), scope = accepted.pin.objectId;
      const selectedBinding = await this.selectedBinding(accepted.value.callerPrincipalId, request);
      let input: Document<'chat-bridge/input'> | null;
      try { input = await this.originalInput(admitted.identity, admitted.requestHash); }
      catch (error) {
        if (!(error instanceof IvyError && error.code === 'mutation_conflict')) throw error;
        const winner = await this.store.named('chat-bridge/input', inputName(this.admission.definition, admitted.identity));
        requireThat(winner && winner.value.operationId !== accepted.value.operationId && same(winner.value.identity, admitted.identity),
          'chat_identity_conflict', 'An original input conflict needs explicit integrity recovery.');
        try {
          await this.operations.rejectUnapplied(scope, 'mutation_conflict', 'The original message identity names different accepted content.');
          await this.writeMain(main, { ...main.value, pendingAction: null, updatedAt: accepted.value.createdAt }, scope, 'release-conflict');
          return;
        } catch (failure) { if (!contention(failure)) throw failure; continue; }
      }
      const current = await this.operations.find(accepted.value.callerPrincipalId, accepted.value.operationId);
      requireThat(current, 'chat_identity_conflict', 'The claimed operation must remain discoverable.');
      if (current.value.phase === 'succeeded') {
        requireThat(input && current.value.outcome?.action === 'send' && same(current.value.outcome.input, { object: input.pin, data: input.value }),
          'chat_identity_conflict', 'The completed admission must retain its exact original input receipt.');
        try { await this.writeMain(main, { ...main.value, pendingAction: null, updatedAt: accepted.value.createdAt }, scope, 'release-completed'); return; }
        catch (error) { if (!contention(error)) throw error; continue; }
      }
      if (!input && (current.value.phase === 'failed' || !same(main.value.binding, selectedBinding) || main.value.queue.length >= 64 || !Number.isSafeInteger(main.value.nextSequence + 1))) {
        try {
          if (current.value.phase !== 'failed') {
            const changed = !same(main.value.binding, selectedBinding);
            await this.operations.rejectUnapplied(scope, changed ? 'chat_binding_mismatch' : 'chat_queue_full', changed ? 'Main changed before admission.' : 'The Main execution queue is full.');
          } else requireThat(current.value.error?.outcome === 'not_executed', 'chat_operation_unresolved', 'An uncertain failed action cannot release its claim.');
          await this.writeMain(main, { ...main.value, pendingAction: null, updatedAt: accepted.value.createdAt }, scope, 'release-refusal');
          return;
        } catch (error) { if (!contention(error)) throw error; continue; }
      }
      // A concurrent original admission may finish between this action's lookup and claim.
      // Recover its receipt even if the native worker has already removed the old ticket.
      if (input && input.value.operationId !== accepted.value.operationId) {
        const owner = await this.operations.find(accepted.value.callerPrincipalId, input.value.operationId);
        requireThat(owner?.value.phase === 'succeeded' && owner.value.outcome?.action === 'send' && owner.value.outcome.input.object.objectId === input.pin.objectId,
          'chat_publication_pending', 'The original message admission must finish before an alias action.');
        try {
          await this.operations.finish(scope, { action: 'send', operationId: accepted.value.operationId, input: { object: input.pin, data: input.value } });
          await this.writeMain(main, { ...main.value, pendingAction: null, updatedAt: accepted.value.createdAt }, scope, 'release-alias');
          return;
        } catch (error) { if (!contention(error)) throw error; continue; }
      }
      requireThat(same(main.value.binding, selectedBinding), 'chat_binding_mismatch', 'A pending send cannot change its selected Main.');
      await this.binding(selectedBinding);
      const ticket = main.value.queue.find(value => same(value.identity, admitted.identity));
      try {
        if (ticket) {
          requireThat(input && ticket.inputId === input.pin.objectId && ticket.requestHash === admitted.requestHash && same(ticket.binding, input.value.binding) &&
            ticket.sequence === input.value.sequence && ticket.admittedAt === input.value.admittedAt, 'chat_identity_conflict', 'The queued ticket differs from its original input.');
        } else {
          requireThat(main.value.queue.length < 64 && Number.isSafeInteger(main.value.nextSequence + 1), 'chat_queue_full', 'The Main execution queue cannot accept another input.');
          if (!input) {
            const at = accepted.value.createdAt, value: Chat.Input = { schemaVersion: 1, workspaceId: this.admission.definition.workspaceId, definitionHash: this.admission.definitionHash,
              identity: admitted.identity, operationId: accepted.value.operationId, requestHash: admitted.requestHash, binding: selectedBinding,
              sequence: main.value.nextSequence, payload: request.payload, state: 'queued', nativeCalls: { resume: null, turn: null, interrupt: null },
              turnId: null, epoch: null, result: null, error: null, cancellation: null, admittedAt: at, updatedAt: at, finishedAt: null };
            const pin = await this.store.write('chat-bridge/input', value, mutation(scope, 'publish-input'),
              { create: { parentId: this.store.rootObjectId, name: inputName(this.admission.definition, admitted.identity) } });
            input = await this.store.read('chat-bridge/input', pin);
          }
          requireThat(input.value.sequence === main.value.nextSequence && input.value.operationId === accepted.value.operationId,
            'chat_identity_conflict', 'An unfinished input cannot consume another admission sequence.');
          const next: Chat.Main = { ...main.value, nextSequence: main.value.nextSequence + 1, updatedAt: accepted.value.createdAt,
            queue: [...main.value.queue, { inputId: input.pin.objectId, identity: admitted.identity, requestHash: admitted.requestHash,
              binding: selectedBinding, sequence: input.value.sequence, admittedAt: input.value.admittedAt }] };
          main = await this.writeMain(main, next, scope, 'publish-ticket');
        }
        requireThat(input, 'chat_identity_conflict', 'A queued input must exist before completing admission.');
        await this.operations.finish(scope, { action: 'send', operationId: accepted.value.operationId, input: { object: input.pin, data: input.value } });
        // Keep the claim until both the queue and the original action receipt are durable.
        await this.writeMain(main, { ...main.value, pendingAction: null, updatedAt: accepted.value.createdAt }, scope, 'release-claim');
        return;
      } catch (error) { if (!contention(error)) throw error; }
    }
    throw new IvyError('chat_contention', 'Main changed repeatedly. Recover its original pending action.', 'unknown');
  }
  async send(caller: string, original: Chat.SendRequest): Promise<Chat.Operation> {
    const admitted = this.admission.send(caller, original), request = structuredClone(original);
    // Refuse known stale/full/missing-Main requests before retaining a new action. A race after
    // this read is resolved under the Main claim, with a durable not-executed receipt.
    if (!await this.operations.find(caller, request.operationId)) {
      const selectedBinding = await this.selectedBinding(caller, request);
      await this.recoverPending();
      if (!await this.originalInput(admitted.identity, admitted.requestHash)) {
        const main = await this.main();
        requireThat(same(main.value.binding, selectedBinding), 'chat_binding_mismatch', 'The displayed Main binding has changed.');
        requireThat(main.value.queue.length < 64 && Number.isSafeInteger(main.value.nextSequence + 1), 'chat_queue_full', 'The Main execution queue is full.');
        await this.binding(selectedBinding);
      }
    }
    const operation = await this.operations.begin(caller, request);
    if (operation.value.phase === 'succeeded' || operation.value.phase === 'failed') return operation.value;
    requireThat(operation.value.phase === 'accepted' || operation.value.phase === 'applying', 'chat_operation_unresolved', 'Recover the original action before accepting another effect.');
    for (let attempt = 0; attempt < 32; attempt++) {
      await this.recoverPending();
      const recovered = await this.operations.find(caller, request.operationId);
      if (recovered?.value.phase === 'succeeded' || recovered?.value.phase === 'failed') return recovered.value;
      const main = await this.main();
      if (main.value.pendingAction) continue;
      try { await this.writeMain(main, { ...main.value, pendingAction: operation.pin, updatedAt: operation.value.createdAt }, operation.pin.objectId, 'claim'); }
      catch (error) { if (!contention(error)) throw error; }
    }
    throw new IvyError('chat_contention', 'Main changed repeatedly. Recover the original input operation.', 'unknown');
  }
}
