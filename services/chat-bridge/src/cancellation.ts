import { canonical, hashJson } from '../../../packages/sdk/src/node.js';
import { IvyError, requireThat } from '../../../packages/sdk/src/node.js';
import type { Chat } from '../../../packages/sdk/src/node.js';
import { validateChat } from '../../../packages/sdk/src/node.js';
import type { ChatMain } from './chat-main.js';
import { ChatQueries } from './queries.js';
import { ChatNotices } from './notices.js';
import { contention } from './operations.js';
import { mutation } from './store.js';
import type { Document } from './store.js';

const same = (a: unknown, b: unknown) => canonical(a) === canonical(b);
type Main = Document<'chat-bridge/main'>;

/** Original cancellation admission. A receipt confirms intent; native settlement is separate. */
export class ChatCancellation {
  readonly queries: ChatQueries;
  constructor(readonly main: ChatMain) { this.queries = new ChatQueries(main.operations); }
  get store() { return this.main.store; }
  private async writeMain(current: Main, next: Chat.Main, scope: string): Promise<void> {
    await this.main.native.verifyOwner();
    await this.store.write('chat-bridge/main', next, mutation(scope, 'cancel-main:' + current.pin.revision + ':' + hashJson(next)),
      { objectId: current.pin.objectId, expectedRevision: current.pin.revision });
  }
  async cancel(caller: string, original: Chat.CancelRequest): Promise<Chat.Operation> {
    this.main.admission.authorize(caller, original.expectedBridge); validateChat('CancelRequest', original);
    const request = structuredClone(original), prior = await this.main.operations.find(caller, request.operationId);
    if (!prior) {
      const input = await this.queries.input(caller, { expectedBridge: request.expectedBridge, inputId: request.inputId });
      requireThat(input.object.revision === request.expectedRevision, 'revision_conflict', 'Cancellation requires the displayed input revision.');
      requireThat(!input.data.finishedAt && !input.data.result && !input.data.cancellation, 'chat_cancel_unavailable', 'Only an unfinished input without another cancellation can accept this intent.');
      const current = await this.main.queue.main();
      requireThat(current.value.queue.some(ticket => ticket.inputId === request.inputId), 'chat_cancel_unavailable', 'The original input must still belong to the execution queue.');
    }
    const operation = await this.main.operations.begin(caller, request);
    for (let attempt = 0; attempt < 24; attempt++) {
      const current = await this.main.queue.main(), latest = await this.main.operations.find(caller, request.operationId);
      requireThat(latest, 'chat_identity_conflict', 'The original cancellation must remain discoverable.');
      if (current.value.pendingAction?.objectId === operation.pin.objectId) {
        await this.recoverPending(); return (await this.main.operations.find(caller, request.operationId))!.value;
      }
      if (latest.value.phase === 'succeeded' || latest.value.phase === 'failed') return latest.value;
      if (current.value.pendingAction) { await this.main.recoverPending(); continue; }
      requireThat(latest.value.phase === 'accepted' && latest.value.nativeCall === null, 'chat_operation_unresolved', 'Only the original unapplied cancellation can claim Main.');
      try { await this.writeMain(current, { ...current.value, pendingAction: operation.pin, updatedAt: operation.value.createdAt }, operation.pin.objectId); }
      catch (error) { if (!contention(error)) throw error; }
    }
    throw new IvyError('chat_contention', 'Recover the original cancellation after concurrent Main actions.', 'unknown');
  }
  async recoverPending(): Promise<void> {
    for (let attempt = 0; attempt < 24; attempt++) {
      const main = await this.main.queue.main(); if (!main.value.pendingAction) return;
      const accepted = await this.store.read('chat-bridge/operation', main.value.pendingAction); this.main.operations.check(accepted.value);
      requireThat(accepted.value.phase === 'accepted' && accepted.value.request.action === 'cancel', 'chat_pending_action', 'This Main claim must retain its original accepted cancellation.');
      const request = accepted.value.request, caller = accepted.value.callerPrincipalId, scope = accepted.pin.objectId;
      const operation = await this.store.read('chat-bridge/operation', scope); this.main.operations.check(operation.value);
      requireThat(same(operation.value.request, request) && operation.value.createdAt === accepted.value.createdAt, 'chat_identity_conflict', 'Cancellation cannot change its original request and time.');
      try {
        const selected = await this.queries.input(caller, { expectedBridge: request.expectedBridge, inputId: request.inputId });
        const cancellation = { operationId: request.operationId, callerPrincipalId: caller, reason: request.reason, requestedAt: accepted.value.createdAt };
        if (operation.value.phase === 'failed' || selected.object.revision !== request.expectedRevision && !same(selected.data.cancellation, cancellation)) {
          requireThat(!same(selected.data.cancellation, cancellation), 'chat_operation_unresolved', 'An applied original cancellation cannot be discarded as unexecuted.');
          if (operation.value.phase !== 'failed') await this.main.operations.rejectUnapplied(scope, 'revision_conflict', 'The input changed before the original cancellation intent was saved.');
          await this.writeMain(main, { ...main.value, pendingAction: null, updatedAt: accepted.value.createdAt }, scope); return;
        }
        const base = await this.store.read('chat-bridge/input', { objectId: request.inputId, revision: request.expectedRevision });
        this.main.admission.authorize(caller, request.expectedBridge, base.value.identity.channel);
        requireThat(!base.value.finishedAt && !base.value.result && !base.value.cancellation && same(base.value.identity, selected.data.identity) && same(base.value.binding, selected.data.binding),
          'chat_identity_conflict', 'Cancellation must preserve its original unfinished input and binding.');
        const intent: Chat.Input = { ...base.value, state: 'cancel_requested', cancellation, updatedAt: accepted.value.createdAt };
        await this.main.native.verifyOwner();
        const pin = await this.store.write('chat-bridge/input', intent, mutation(scope, 'cancel-intent'), { objectId: request.inputId, expectedRevision: request.expectedRevision });
        const saved = await this.store.read('chat-bridge/input', pin);
        await this.main.operations.finish(scope, { action: 'cancel', operationId: request.operationId, input: { object: saved.pin, data: saved.value } });
        // No attached call means no worker could already have passed its native dispatch guard.
        if (!base.value.turnId && Object.values(base.value.nativeCalls).every(value => value === null)) {
          const cancelled = await this.store.write('chat-bridge/input', { ...intent, state: 'cancelled', finishedAt: accepted.value.createdAt, error: null },
            mutation(scope, 'cancel-queued'), { objectId: request.inputId, expectedRevision: saved.pin.revision });
          await new ChatNotices(this.main).finishUndelivered(await this.store.read('chat-bridge/input', cancelled));
          await this.writeMain(main, { ...main.value, queue: main.value.queue.filter(ticket => ticket.inputId !== request.inputId), pendingAction: null, updatedAt: accepted.value.createdAt }, scope);
        } else await this.writeMain(main, { ...main.value, pendingAction: null, updatedAt: accepted.value.createdAt }, scope);
        return;
      } catch (error) { if (!contention(error)) throw error; }
    }
    throw new IvyError('chat_contention', 'Recover the original claimed cancellation after input contention.', 'unknown');
  }
  async recover(input: Chat.Input, inputId: string): Promise<Chat.Operation> {
    const cancellation = input.cancellation; requireThat(cancellation, 'chat_cancel_unavailable', 'The input must retain its original cancellation intent.');
    const operation = await this.main.operations.find(cancellation.callerPrincipalId, cancellation.operationId);
    requireThat(operation?.value.request.action === 'cancel' && operation.value.request.inputId === inputId && operation.value.request.reason === cancellation.reason &&
      operation.value.createdAt === cancellation.requestedAt, 'chat_identity_conflict', 'Cancellation must preserve its exact original caller, request and reason.');
    return this.cancel(cancellation.callerPrincipalId, operation.value.request);
  }
}
