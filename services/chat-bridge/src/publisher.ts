import { canonical, hashJson } from '../../../packages/sdk/src/node.js';
import { requireThat } from '../../../packages/sdk/src/node.js';
import type { Chat } from '../../../packages/sdk/src/node.js';
import { ChatCollector } from './collector.js';
import { readChatResultText, chatReplyParts } from './result-text.js';
import { contention } from './operations.js';
import { mutation } from './store.js';
import type { Document } from './store.js';
import { attributedNoticeReply } from './notice-input.js';

const same = (a: unknown, b: unknown) => canonical(a) === canonical(b);
type Main = Document<'chat-bridge/main'>;
type Material = { collection: Chat.ObjectPin; result: Document<'chat-bridge/result'>; parts: ReturnType<typeof chatReplyParts> };
const originalClaim = ({ publishedParts: _progress, ...value }: Chat.ResultPublication) => value;

/** One durable publication step: reserve, finish Input, save bounded reply parts, release head.
 * Native collection is independently complete before this class obtains a publication claim. */
export class ChatPublisher {
  private cache: Material | null = null;
  constructor(readonly collector: ChatCollector) {}
  get main() { return this.collector.main; }
  get store() { return this.main.store; }
  private async material(inputId: string): Promise<Material | null> {
    const selected = await this.collector.find(inputId);
    if (!selected || selected.value.state !== 'complete') return null;
    if (this.cache && this.cache.result.value.inputId === inputId && same(selected.pin, this.cache.collection) && same(selected.value.result, this.cache.result.pin)) return this.cache;
    const verified = await this.collector.step(inputId);
    requireThat(verified?.value.state === 'complete' && same(verified.pin, selected.pin), 'revision_conflict', 'Publication must pin the unchanged complete original collection.');
    const result = await this.store.read('chat-bridge/result', verified.value.result!, inputId), input = await this.store.read('chat-bridge/input', inputId);
    let text = await readChatResultText(this.store, inputId, result.value);
    const operation = await this.main.operations.find(input.value.identity.senderPrincipalId, input.value.operationId);
    requireThat(operation, 'chat_identity_conflict', 'Publication requires the input\'s original operation.');
    if (operation.value.request.action === 'notify') text = attributedNoticeReply(operation.value.callerPrincipalId, text);
    this.cache = { collection: verified.pin, result, parts: chatReplyParts(result.value, text) }; return this.cache;
  }
  private async writeMain(current: Main, next: Chat.Main): Promise<void> {
    await this.main.native.verifyOwner();
    await this.store.write('chat-bridge/main', next, mutation(current.pin.objectId, 'publish:' + current.pin.revision + ':' + hashJson(next)),
      { objectId: current.pin.objectId, expectedRevision: current.pin.revision });
  }
  private async claimed(claim: Chat.ResultPublication): Promise<Main> {
    const current = await this.main.queue.main();
    requireThat(!current.value.pendingAction && current.value.queue[0]?.inputId === claim.inputId && current.value.publication &&
      same(originalClaim(current.value.publication), originalClaim(claim)), 'revision_conflict', 'Only the current original publication may finish its queue head.');
    return current;
  }
  private terminal(input: Chat.Input, result: Chat.Result): Pick<Chat.Input, 'state' | 'error' | 'finishedAt' | 'updatedAt'> {
    const completed = result.nativeState === 'completed', cancelled = result.nativeState === 'interrupted' && input.cancellation !== null;
    return { state: completed ? 'completed' : cancelled ? 'cancelled' : 'failed', finishedAt: result.createdAt, updatedAt: result.createdAt,
      error: completed || cancelled ? null : { code: 'chat_native_turn_' + result.nativeState, outcome: 'unknown', detail: 'The original native turn ' + result.nativeState + '. Its complete result is retained.' } };
  }
  private async finishNotice(input: Document<'chat-bridge/input'>, claim: Chat.ResultPublication): Promise<void> {
    const operation = await this.main.operations.find(input.value.identity.senderPrincipalId, input.value.operationId);
    requireThat(operation, 'chat_identity_conflict', 'Publication requires the input\'s original operation.');
    if (operation.value.request.action !== 'notify') return;
    const reply = await this.store.named('chat-bridge/reply', 'Chat reply ' + input.pin.objectId + ' part 0');
    requireThat(reply && reply.value.origin.kind === 'native' && reply.value.origin.inputId === input.pin.objectId &&
      same(reply.value.origin.result, claim.result) && same(reply.value.channel, operation.value.request.channel),
      'chat_identity_conflict', 'A completed service handoff needs Main\'s exact first published reply.');
    await this.main.operations.finish(operation.pin.objectId,
      { action: 'notify', operationId: operation.value.operationId, reply: { object: reply.pin, data: reply.value } });
  }
  async step(): Promise<void> {
    try {
      await this.main.recoverPending();
      const head = await this.collector.delivery.head(); if (!head || !head.input.value.turnId) return;
      const material = await this.material(head.input.pin.objectId); if (!material) return;
      let current = await this.main.queue.main();
      requireThat(!current.value.pendingAction && current.value.queue[0]?.inputId === head.input.pin.objectId, 'revision_conflict', 'Publication requires its unchanged current queue head.');
      const { result, parts } = material;
      if (!current.value.publication) {
        requireThat(Number.isSafeInteger(current.value.nextSequence + parts.length), 'chat_sequence_exhausted', 'The complete reply range cannot exceed the workspace sequence bound.');
        const publication: Chat.ResultPublication = { inputId: head.input.pin.objectId, collection: material.collection, result: result.pin,
          firstSequence: current.value.nextSequence, partCount: parts.length, publishedParts: 0, createdAt: result.value.createdAt };
        await this.writeMain(current, { ...current.value, publication, nextSequence: current.value.nextSequence + parts.length, updatedAt: publication.createdAt }); return;
      }
      const claim = current.value.publication;
      requireThat(same(claim.collection, material.collection) && same(claim.result, result.pin) && claim.partCount === parts.length && claim.createdAt === result.value.createdAt &&
        same(result.value.binding, head.input.value.binding) && result.value.turnId === head.input.value.turnId,
        'chat_identity_conflict', 'Publication must keep its original complete result, binding, turn and reply parts.');
      const terminal = this.terminal(head.input.value, result.value);
      if (!head.input.value.result) {
        requireThat(!head.input.value.finishedAt, 'chat_identity_conflict', 'A different terminal input cannot be replaced by a collected result.');
        await this.main.native.verifyOwner(); await this.claimed(claim);
        await this.store.write('chat-bridge/input', { ...head.input.value, ...terminal, result: result.pin },
          mutation(head.input.pin.objectId, 'complete:' + head.input.pin.revision + ':' + hashJson({ terminal, result: result.pin })),
          { objectId: head.input.pin.objectId, expectedRevision: head.input.pin.revision }); return;
      }
      requireThat(same(head.input.value.result, result.pin) && Object.entries(terminal).every(([key, value]) => same(head.input.value[key as keyof Chat.Input], value)),
        'chat_identity_conflict', 'All replies require the original input terminal state and complete Result.');
      if (claim.publishedParts === claim.partCount) {
        current = await this.claimed(claim);
        await this.finishNotice(head.input, claim);
        await this.writeMain(current, { ...current.value, publication: null, queue: current.value.queue.slice(1), updatedAt: claim.createdAt }); this.cache = null; return;
      }
      const end = Math.min(claim.partCount, claim.publishedParts + 8);
      for (let index = claim.publishedParts; index < end; index++) {
        await this.main.native.verifyOwner(); await this.claimed(claim);
        const immutable = { schemaVersion: 1 as const, workspaceId: head.input.value.workspaceId, definitionHash: head.input.value.definitionHash,
          channel: head.input.value.identity.channel, origin: { kind: 'native' as const, inputId: claim.inputId, binding: head.input.value.binding, result: result.pin },
          sequence: claim.firstSequence + index, partIndex: index, ...parts[index]!, createdAt: claim.createdAt };
        const value: Chat.Reply = { ...immutable, immutableHash: hashJson(immutable),
          state: 'pending', firstOfferedAt: null, confirmedAt: null };
        await this.store.write('chat-bridge/reply', value, mutation(claim.inputId, 'reply:' + result.pin.objectId + ':' + index),
          { create: { parentId: this.store.rootObjectId, name: 'Chat reply ' + claim.inputId + ' part ' + index } });
      }
      current = await this.claimed(claim);
      if (current.value.publication!.publishedParts !== claim.publishedParts) return;
      await this.writeMain(current, { ...current.value, publication: { ...claim, publishedParts: end }, updatedAt: claim.createdAt });
    } catch (error) { if (!contention(error)) throw error; }
  }
}
