import { canonical, hashJson } from '../../../packages/sdk/src/node.js';
import { IvyError, requireThat } from '../../../packages/sdk/src/node.js';
import type { Chat } from '../../../packages/sdk/src/node.js';
import { validateChat } from '../../../packages/sdk/src/node.js';
import { mainNotification } from '../../../packages/sdk/src/node.js';
import type { ChatMain } from './chat-main.js';
import { admittedNoticeInput } from './notice-input.js';
import { contention } from './operations.js';
import { ChatOutbox } from './outbox.js';
import { inputName } from './queue.js';
import { mutation } from './store.js';
import type { Document } from './store.js';

const same = (a: unknown, b: unknown) => canonical(a) === canonical(b);
type InputDocument = Document<'chat-bridge/input'>;

/** Durable service handoff. The service can only enqueue context for the current Main;
 * only Main's native answer is published to the user. */
export class ChatNotices {
  constructor(readonly main: ChatMain) {}
  get operations() { return this.main.operations; }
  get store() { return this.main.store; }
  private async input(operation: Document<'chat-bridge/operation'>): Promise<InputDocument | null> {
    const request = operation.value.request;
    requireThat(request.action === 'notify', 'chat_identity_conflict', 'A service handoff needs its original notice operation.');
    const identity: Chat.InputIdentity = { channel: structuredClone(request.channel), senderPrincipalId: operation.value.callerPrincipalId, messageId: request.operationId };
    const input = await this.store.named('chat-bridge/input', inputName(this.main.admission.definition, identity, operation.value.definitionHash));
    if (!input) return null;
    const admitted = admittedNoticeInput(this.main.admission, operation.value.callerPrincipalId, request, input.value.binding);
    requireThat(input.value.workspaceId === operation.value.workspaceId && input.value.definitionHash === operation.value.definitionHash &&
      same(input.value.identity, admitted.identity) && input.value.operationId === operation.value.operationId && input.value.requestHash === admitted.requestHash &&
      same(input.value.payload, admitted.payload), 'chat_identity_conflict', 'The retained service handoff differs from its original notice operation.');
    return input;
  }
  async notify(caller: string, original: Chat.NotifyRequest): Promise<Chat.Operation> {
    validateChat('NotifyRequest', original);
    const previous = await this.operations.find(caller, original.operationId);
    if (previous) {
      requireThat(previous.value.request.action === 'notify' && same(mainNotification(previous.value.request), original),
        'mutation_conflict', 'This service operation already names different notification content.');
      // Lookup stays tied to the saved input even after native plan or transport configuration changes.
      if (['succeeded', 'failed', 'outcome_unknown'].includes(previous.value.phase) || await this.input(previous)) return previous.value;
    }
    const request: Chat.AdmittedNotifyRequest = previous?.value.request.action === 'notify' ? previous.value.request : {
      ...structuredClone(original), expectedBridge: this.main.admission.expected(caller, 'notice'),
      channel: structuredClone(this.main.admission.definition.channels[0]!.channel),
    };
    await this.main.native.verifyOwner();
    if (!await this.operations.find(caller, request.operationId)) {
      await this.main.ensure(caller, request.operationId, 'notice');
      await this.main.recoverPending();
      const main = await this.main.queue.main();
      requireThat(main.value.binding, 'chat_main_missing', 'The automatic Main creation did not publish a binding.');
      requireThat(main.value.queue.length < 64 && Number.isSafeInteger(main.value.nextSequence + 1), 'chat_queue_full', 'The Main execution queue cannot accept another service handoff.');
      await this.store.requireAvailable(request.source);
    }
    const accepted = await this.operations.begin(caller, request);
    for (let attempt = 0; attempt < 32; attempt++) {
      await this.main.recoverPending();
      const operation = await this.operations.find(caller, request.operationId);
      requireThat(operation, 'chat_identity_conflict', 'The original notice operation must remain discoverable.');
      if (['succeeded', 'failed', 'outcome_unknown'].includes(operation.value.phase)) return operation.value;
      requireThat(operation.value.phase === 'accepted', 'chat_operation_unresolved', 'Recover the original service handoff.');
      if (await this.input(operation)) return operation.value;
      const main = await this.main.queue.main();
      if (main.value.pendingAction) continue;
      const next = { ...main.value, pendingAction: accepted.pin, updatedAt: accepted.value.createdAt };
      try {
        await this.main.native.verifyOwner();
        await this.store.write('chat-bridge/main', next, mutation(accepted.pin.objectId, 'notice-claim:' + main.pin.revision + ':' + hashJson(next)),
          { objectId: main.pin.objectId, expectedRevision: main.pin.revision });
      } catch (error) { if (!contention(error)) throw error; }
    }
    throw new IvyError('chat_contention', 'Recover the original service handoff after concurrent Main actions.', 'unknown');
  }
  async recoverPending(): Promise<void> {
    for (let attempt = 0; attempt < 32; attempt++) {
      let main = await this.main.queue.main(); if (!main.value.pendingAction) return;
      const accepted = await this.store.read('chat-bridge/operation', main.value.pendingAction); this.operations.check(accepted.value);
      const request = accepted.value.request, scope = accepted.pin.objectId;
      requireThat(accepted.value.phase === 'accepted' && request.action === 'notify',
        'chat_pending_action', 'Service handoff recovery requires its original accepted Main claim.');
      let input = await this.input(accepted);
      try {
        if (!input && (!main.value.binding || main.value.queue.length >= 64 || !Number.isSafeInteger(main.value.nextSequence + 1))) {
          await this.operations.rejectUnapplied(scope, main.value.binding ? 'chat_queue_full' : 'chat_main_missing',
            main.value.binding ? 'The Main execution queue is full.' : 'No current Main binding can receive the service handoff.');
          main = await this.main.queue.main();
          await this.store.write('chat-bridge/main', { ...main.value, pendingAction: null, updatedAt: accepted.value.createdAt },
            mutation(scope, 'notice-release-refusal:' + main.pin.revision), { objectId: main.pin.objectId, expectedRevision: main.pin.revision }); return;
        }
        requireThat(main.value.binding, 'chat_main_missing', 'A service handoff needs the current Main binding.');
        const admitted = admittedNoticeInput(this.main.admission, accepted.value.callerPrincipalId, request, main.value.binding);
        const ticket = main.value.queue.find(value => same(value.identity, admitted.identity));
        if (ticket) {
          requireThat(input && ticket.inputId === input.pin.objectId && ticket.requestHash === admitted.requestHash && same(ticket.binding, input.value.binding) &&
            ticket.sequence === input.value.sequence && ticket.admittedAt === input.value.admittedAt,
            'chat_identity_conflict', 'The queued service handoff differs from its original input.');
        } else {
          requireThat(main.value.queue.length < 64 && Number.isSafeInteger(main.value.nextSequence + 1), 'chat_queue_full', 'The Main execution queue cannot accept another service handoff.');
          if (!input) {
            const at = accepted.value.createdAt, value: Chat.Input = { schemaVersion: 1, workspaceId: this.main.admission.definition.workspaceId,
              definitionHash: this.main.admission.definitionHash, identity: admitted.identity, operationId: accepted.value.operationId,
              requestHash: admitted.requestHash, binding: main.value.binding, sequence: main.value.nextSequence, payload: admitted.payload,
              state: 'queued', nativeCalls: { resume: null, turn: null, interrupt: null }, turnId: null, epoch: null, result: null, error: null,
              cancellation: null, admittedAt: at, updatedAt: at, finishedAt: null };
            const pin = await this.store.write('chat-bridge/input', value, mutation(scope, 'publish-notice-input'),
              { create: { parentId: this.store.rootObjectId, name: inputName(this.main.admission.definition, admitted.identity) } });
            input = await this.store.read('chat-bridge/input', pin);
          }
          requireThat(input.value.sequence === main.value.nextSequence && input.value.operationId === accepted.value.operationId,
            'chat_identity_conflict', 'An unfinished service handoff cannot consume another admission sequence.');
          const next: Chat.Main = { ...main.value, nextSequence: main.value.nextSequence + 1, updatedAt: accepted.value.createdAt,
            queue: [...main.value.queue, { inputId: input.pin.objectId, identity: admitted.identity, requestHash: admitted.requestHash,
              binding: main.value.binding, sequence: input.value.sequence, admittedAt: input.value.admittedAt }] };
          const pin = await this.store.write('chat-bridge/main', next, mutation(scope, 'publish-notice-ticket:' + main.pin.revision + ':' + hashJson(next)),
            { objectId: main.pin.objectId, expectedRevision: main.pin.revision });
          main = await this.store.read('chat-bridge/main', pin);
        }
        await this.store.write('chat-bridge/main', { ...main.value, pendingAction: null, updatedAt: accepted.value.createdAt },
          mutation(scope, 'notice-release-claim:' + main.pin.revision), { objectId: main.pin.objectId, expectedRevision: main.pin.revision }); return;
      } catch (error) { if (!contention(error)) throw error; }
    }
    throw new IvyError('chat_contention', 'The original service handoff still needs queue recovery.', 'unknown');
  }
  async notice(caller: string, request: Chat.NoticeQuery): Promise<Chat.ReplyView> {
    validateChat('NoticeQuery', request);
    const operation = await this.operations.find(caller, request.operationId);
    requireThat(operation?.value.request.action === 'notify', 'not_found', 'This producer has no notice with this original identity.');
    const outcome = operation.value.outcome;
    requireThat(operation.value.phase !== 'failed', operation.value.error?.code ?? 'chat_operation_failed', operation.value.error?.detail ?? 'The service handoff failed.');
    requireThat(operation.value.phase === 'succeeded' && outcome?.action === 'notify', 'chat_publication_pending', 'Main has not finished the original service handoff.');
    const reply = await new ChatOutbox(this.operations).reply(outcome.reply.object.objectId, operation.value.request.channel);
    const input = await this.input(operation);
    requireThat(input && reply.value.origin.kind === 'native' && reply.value.origin.inputId === input.pin.objectId,
      'chat_identity_conflict', 'Notice delivery must be Main\'s answer to this exact service handoff.');
    return { object: reply.pin, data: reply.value };
  }
  /** Settle the producer receipt before releasing an input that never started a native turn. */
  async finishUndelivered(input: InputDocument): Promise<void> {
    const operation = await this.operations.find(input.value.identity.senderPrincipalId, input.value.operationId);
    requireThat(operation, 'chat_identity_conflict', 'An undelivered input needs its original operation.');
    if (operation.value.request.action !== 'notify') return;
    const original = await this.input(operation);
    requireThat(original && same(original.pin, input.pin) && input.value.finishedAt && !input.value.turnId && !input.value.result &&
      ['failed', 'cancelled'].includes(input.value.state), 'chat_identity_conflict', 'Only an unchanged terminal undelivered handoff can fail its producer receipt.');
    const error: Chat.Error = input.value.error ?? { code: 'chat_notice_cancelled', detail: 'The service handoff was cancelled before its native turn.', outcome: 'not_executed' };
    if (operation.value.phase === 'failed') {
      requireThat(same(operation.value.error, error), 'chat_identity_conflict', 'The original handoff failure cannot change.'); return;
    }
    requireThat(operation.value.phase === 'accepted', 'chat_operation_unresolved', 'An undelivered handoff must retain its original accepted operation.');
    const next: Chat.Operation = { ...operation.value, phase: 'failed', error, updatedAt: input.value.finishedAt };
    await this.store.write('chat-bridge/operation', next, mutation(operation.pin.objectId, 'undelivered:' + hashJson(next)),
      { objectId: operation.pin.objectId, expectedRevision: operation.pin.revision });
  }
}
