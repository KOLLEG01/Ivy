import { canonical, digest, hashJson } from '../../../packages/sdk/src/node.js';
import { IvyError, requireThat } from '../../../packages/sdk/src/node.js';
import type { Chat, Wire } from '../../../packages/sdk/src/node.js';
import { ChatMain } from './chat-main.js';
import { ChatDelivery } from './delivery.js';
import { verifyChatNativeRead } from './native-evidence.js';
import { chunkBytes, maximumEvidenceBytes } from './evidence-bytes.js';
import { ChatCollectionReads, collectionBytes, collectionReads } from './collection-reads.js';
import { contention } from './operations.js';
import { mutation, pinOf } from './store.js';
import type { Document } from './store.js';
import { readChatResultText } from './result-text.js';
import { scopedOperationId } from '../../../packages/sdk/src/client.js';
import { deriveOperationId } from '../../../packages/sdk/src/node.js';

const same = (a: unknown, b: unknown) => canonical(a) === canonical(b);
type Collection = Document<'chat-bridge/collection'>;
export interface CollectionRetry { inputId: string; expectedCollection: Chat.ObjectPin; expectedBridge: Chat.ExpectedBridge; operationId: string; reason: string }
const name = 'Chat original turn collection';

/** Read-only native collection; no method here dispatches a native mutation or releases a queue
 * ticket. Every completed Result is reconstructed from retained original read evidence. */
export class ChatCollector {
  readonly delivery: ChatDelivery;
  private cache: { pin: Chat.ObjectPin; state: ChatCollectionReads } | null = null;
  constructor(readonly main: ChatMain) { this.delivery = new ChatDelivery(main); }
  get store() { return this.main.store; }
  get native() { return this.main.native; }
  async find(inputId: string): Promise<Collection | null> { return this.store.named('chat-bridge/collection', name, inputId); }
  async observedTerminal(inputId: string): Promise<boolean> {
    const source = await this.source(inputId), current = await this.find(inputId); if (!current) return false;
    await this.check(current, source);
    let attempt: Collection | null = current;
    for (let count = 0; attempt && count < 32; count++) {
      const state = new ChatCollectionReads(source.binding.value.primary.nativeId, attempt.value.turnId);
      for (const pin of attempt.value.reads) {
        const original = await this.native.evidence.read(inputId, this.expected(attempt.value, state), pin);
        try { state.append(original); } catch { break; }
        if (state.nativeState !== null) return true;
      }
      attempt = attempt.value.previousAttempt ? await this.store.read('chat-bridge/collection', attempt.value.previousAttempt, inputId) : null;
    }
    return false;
  }
  private async source(inputId: string) {
    const selected = await this.delivery.head(inputId);
    requireThat(selected && selected.input.value.turnId && selected.input.value.epoch && selected.input.value.nativeCalls.turn,
      'chat_collection_unstarted', 'Collection requires the original delivered queue head and native turn.');
    const call = await this.native.get(inputId, selected.input.value.nativeCalls.turn);
    requireThat(call.value.operationId === deriveOperationId(selected.input.value.operationId, 'chat:native:turn/start') && call.value.predecessor === null && call.value.preparedEpoch === null &&
      call.value.method === 'turn/start' && call.value.state === 'succeeded' && call.value.evidence && call.value.epoch === selected.input.value.epoch,
      'chat_native_mismatch', 'Collection requires its original succeeded native turn receipt.');
    const operation = await this.native.evidence.operation(inputId, call.value, call.value.evidence);
    const reply = operation.reply;
    requireThat(reply && 'result' in reply && (operation.params as Record<string, Wire.Json>)['threadId'] === selected.binding.value.primary.nativeId &&
      ((reply.result as Record<string, Wire.Json>)['turn'] as Record<string, Wire.Json>)['id'] === selected.input.value.turnId,
      'chat_native_mismatch', 'Only the turn from the original Main call may supply the result.');
    return { ...selected, call };
  }
  private async check(collection: Collection, source: Awaited<ReturnType<ChatCollector['source']>>): Promise<void> {
    const value = collection.value;
    requireThat(collection.metadata.name === name && same(value.input, { objectId: source.input.pin.objectId, revision: 1 }) &&
      same(value.binding, source.input.value.binding) && same(value.turnCall, source.call.pin) && value.turnId === source.input.value.turnId,
      'chat_identity_conflict', 'Collection must retain its original input, binding and succeeded turn call.');
    requireThat(value.attempt>=1&&value.attempt<=32,'chat_identity_conflict','Collection attempt is outside its bounded workflow state.');
    if (value.previousAttempt) {
      requireThat(value.previousAttempt.objectId === collection.pin.objectId && value.previousAttempt.revision < collection.pin.revision,
        'chat_identity_conflict', 'A new read epoch must retain its earlier original collection revision.');
      const retry = value.retryAuthorization;
      requireThat(value.attempt>1&&(!retry||retry.failed.objectId===collection.pin.objectId&&retry.failed.revision===value.previousAttempt.revision),
        'chat_identity_conflict','A new read attempt must identify its immediately preceding workflow state.');
    } else requireThat(value.attempt===1,'chat_identity_conflict','A later read attempt must identify its preceding workflow state.');
  }
  private expected(value: Chat.Collection, state: ChatCollectionReads) {
    const definition = this.main.admission.definition;
    return { serviceNodeId: definition.project.serviceNodeId, callerPrincipalId: definition.principalId,
      nativeVersion: definition.nativePlan.nativeVersion, epoch: value.epoch, ...state.next() };
  }
  private async replay(collection: Collection, threadId: string): Promise<ChatCollectionReads> {
    if (this.cache && same(this.cache.pin, collection.pin)) return this.cache.state;
    const state = new ChatCollectionReads(threadId, collection.value.turnId), pins = new Set<string>();
    for (const pin of collection.value.reads) {
      requireThat(pin.revision === 1 && !pins.has(pin.objectId), 'chat_collection_gap', 'A collection must pin distinct immutable original evidence.'); pins.add(pin.objectId);
      const original = await this.native.evidence.read(collection.value.input.objectId, this.expected(collection.value, state), pin);
      requireThat(state.append(original), 'chat_collection_gap', 'A saved collection must not use an in-progress poll as result evidence.');
    }
    requireThat(state.nativeBytes === collection.value.nativeBytes && (collection.value.state === 'collecting' ? state.phase !== 'done' : state.phase === 'done'),
      'chat_collection_gap', 'The saved collection state and byte total must agree with its full original evidence.');
    this.cache = { pin: collection.pin, state }; return state;
  }
  private async update(current: Collection, next: Chat.Collection): Promise<Collection> {
    const pin = await this.store.write('chat-bridge/collection', next,
      mutation(current.pin.objectId, 'collection:' + current.pin.revision + ':' + hashJson(next)),
      { objectId: current.pin.objectId, expectedRevision: current.pin.revision });
    return this.store.read('chat-bridge/collection', pin, current.value.input.objectId);
  }
  private async fail(current: Collection, error: unknown): Promise<Collection> {
    const failure = IvyError.from(error);
    return this.exposeFailure(await this.update(current, { ...current.value, state: 'failed', result: null,
      error: { code: failure.code, outcome: 'unknown', detail: failure.message.slice(0, 2048) }, updatedAt: new Date().toISOString() }));
  }
  private async exposeFailure(current: Collection): Promise<Collection> {
    if (current.value.state !== 'failed') return current;
    const head = await this.delivery.head(current.value.input.objectId);
    if (!head || head.input.value.finishedAt || head.input.value.result) return current;
    const input = head.input, state = input.value.cancellation ? 'cancel_requested' : 'collecting';
    requireThat(input.value.turnId === current.value.turnId && same(input.value.binding, current.value.binding),
      'chat_collection_mismatch', 'Collection failure belongs to its original input and turn.');
    if (input.value.state === state && same(input.value.error, current.value.error)) return current;
    const next: Chat.Input = { ...input.value, state, error: current.value.error, updatedAt: current.value.updatedAt };
    await this.store.write('chat-bridge/input', next, mutation(current.pin.objectId, 'expose-failure:' + input.pin.revision + ':' + hashJson(next)),
      { objectId: input.pin.objectId, expectedRevision: input.pin.revision });
    return current;
  }
  private async publish(current: Collection, state: ChatCollectionReads): Promise<Collection> {
    requireThat(state.phase === 'done' && state.nativeState && state.lastObservedAt, 'chat_collection_incomplete', 'A Result needs all original native evidence and final verification.');
    const parentId = current.value.input.objectId;
    if (current.value.state === 'complete') {
      const result = await this.store.read('chat-bridge/result', current.value.result!, parentId), { textParts: _parts, ...metadata } = result.value;
      requireThat(result.pin.revision === 1 && same(metadata, { schemaVersion: 1, inputId: parentId, binding: current.value.binding, turnId: current.value.turnId,
        nativeState: state.nativeState, evidence: current.value.reads, createdAt: state.lastObservedAt }) &&
        await readChatResultText(this.store, parentId, result.value) === state.texts.join('\n\n'),
        'chat_evidence_mismatch', 'The completed collection must retain its exact original Result and complete text.'); return current;
    }
    const bytes = Buffer.from(state.texts.join('\n\n')), textParts: Chat.Artifact[] = [];
    requireThat(bytes.length === state.textBytes && bytes.length <= collectionBytes, 'chat_collection_limit', 'The complete UTF-8 text must fit its declared bound.');
    for (let offset = 0; offset < bytes.length;) {
      requireThat(textParts.length < 64, 'chat_collection_limit', 'Complete UTF-8 text exceeds 64 bounded immutable parts.');
      let end = Math.min(bytes.length, offset + chunkBytes);
      while (end < bytes.length && (bytes[end]! & 0xc0) === 0x80) end--;
      const part = bytes.subarray(offset, end), index = textParts.length, contentHash = digest(part);
      const saved = await this.store.client.request('objects.write', { mutationId: await scopedOperationId(this.store.client, ['chat', mutation(parentId, 'result-text:' + index + ':' + contentHash)]), contractVersion: '1.0.0',
        references: {}, create: { parentId, ownerObjectId: parentId, contractKey: 'chat-bridge/result-text', name: 'Chat result text ' + index + ' ' + contentHash.slice(7) },
        content: { encoding: 'text', value: part.toString('utf8') } });
      textParts.push({ object: pinOf(saved), contentHash, byteLength: part.length, mediaType: 'text/plain', label: 'Reply text part ' + (index + 1) }); offset = end;
    }
    const value: Chat.Result = { schemaVersion: 1, inputId: parentId, binding: current.value.binding, turnId: current.value.turnId,
      nativeState: state.nativeState, evidence: current.value.reads, textParts, createdAt: state.lastObservedAt };
    const pin = await this.store.write('chat-bridge/result', value, mutation(current.pin.objectId, 'result:' + current.value.attempt),
      { create: { parentId, name: 'Chat complete result ' + current.pin.objectId + ' attempt ' + current.value.attempt } });
    return this.update(current, { ...current.value, state: 'complete', result: pin, error: null, updatedAt: state.lastObservedAt });
  }
  /** One native read at most, with bounded local recovery/publication. Recreate this worker freely. */
  async step(inputId: string): Promise<Collection | null> {
    const source = await this.source(inputId), threadId = source.binding.value.primary.nativeId;
    let current = await this.find(inputId);
    try {
      if (current) {
        await this.check(current, source);
        if (current.value.state === 'failed') {
          const code = current.value.error?.code, delay = code === 'chat_collection_gap' ? 5000 : 1000 * 2 ** current.value.attempt;
          if (!['chat_collection_native_error', 'chat_collection_gap'].includes(code ?? '') || current.value.attempt >= (code === 'chat_collection_gap' ? 32 : 3) ||
            Date.now() < Date.parse(current.value.updatedAt) + delay) return await this.exposeFailure(current);
          const status = await this.native.ready();
          current = await this.restart(current, status.epoch!);
        }
        if (current.value.state === 'ready' || current.value.state === 'complete') return await this.publish(current, await this.replay(current, threadId));
      }
      const status = await this.native.ready();
      if (!current) {
        const at = new Date().toISOString(), value: Chat.Collection = { schemaVersion: 1, input: { objectId: inputId, revision: 1 }, binding: source.input.value.binding,
          turnCall: source.call.pin, turnId: source.input.value.turnId!, epoch: status.epoch!, attempt: 1, previousAttempt: null, reads: [], nativeBytes: 0,
          state: 'collecting', result: null, error: null, createdAt: at, updatedAt: at };
        const pin = await this.store.write('chat-bridge/collection', value, mutation(inputId, 'collection'), { create: { parentId: inputId, name } });
        current = await this.store.read('chat-bridge/collection', pin, inputId);
      } else if (status.epoch !== current.value.epoch) {
        requireThat(current.value.attempt < 32, 'chat_collection_limit', 'Explicit operator investigation is required after 32 read attempts.');
        current = await this.update(current, { ...current.value, epoch: status.epoch!, attempt: current.value.attempt + 1, previousAttempt: current.pin,
          reads: [], nativeBytes: 0, updatedAt: status.observedAt }); this.cache = null;
      }
      const state = (await this.replay(current, threadId)).fork();
      requireThat(state.count < collectionReads, 'chat_collection_limit', 'Native collection exhausted its complete read bound.');
      const expected = this.expected(current.value, state);
      const observation = await this.native.management('read', { nativeVersion: expected.nativeVersion, method: expected.method, params: expected.params });
      verifyChatNativeRead(observation, expected, this.main.admission.definition.nativePlan.catalogSourceHash);
      const byteLength = Buffer.byteLength(canonical(observation, maximumEvidenceBytes));
      requireThat(current.value.nativeBytes + byteLength <= collectionBytes, 'chat_collection_limit', 'Native collection exhausted its complete byte bound.');
      this.cache = null;
      let failure: unknown;
      try { if (!state.append(observation)) return current; } catch (error) { failure = error; }
      // Even a native error, gap or changed terminal observation is retained before exposing failure.
      const evidence = await this.native.evidence.saveRead(inputId, expected, observation);
      const next: Chat.Collection = { ...current.value, reads: [...current.value.reads, evidence], nativeBytes: current.value.nativeBytes + byteLength,
        state: failure ? 'failed' : state.phase === 'done' ? 'ready' : 'collecting', error: failure ? {
          code: IvyError.from(failure).code, outcome: 'unknown', detail: IvyError.from(failure).message.slice(0, 2048) } : null, updatedAt: observation.observedAt };
      current = await this.update(current, next);
      if (!failure) this.cache = { pin: current.pin, state };
      return await this.exposeFailure(current);
    } catch (error) {
      this.cache = null;
      if (contention(error)) return this.find(inputId);
      // Transport/storage loss retries the original checkpoint. Semantic failures are durable and
      // never converted into a completed Result or an authorization to start another native turn.
      if (current && error instanceof IvyError && (error.code.startsWith('chat_collection_') || error.code === 'chat_result_text_invalid')) return this.fail(current, error);
      throw error;
    }
  }
  private async restart(current: Collection, epoch: string, retryAuthorization?: NonNullable<Chat.Collection['retryAuthorization']>): Promise<Collection> {
    requireThat(current.value.state === 'failed' && current.value.attempt < 32, 'chat_collection_retry_refused', 'Only a bounded failed original read may be retried.');
    this.cache = null;
    return this.update(current, { ...current.value, epoch, attempt: current.value.attempt + 1, previousAttempt: current.pin,
      state: 'collecting', result: null, error: null, reads: [], nativeBytes: 0, retryAuthorization: retryAuthorization ?? null, updatedAt: new Date().toISOString() });
  }
  async retry(caller: string, request: CollectionRetry): Promise<Chat.Collection> {
    this.main.admission.authorize(caller, request.expectedBridge);
    const input = await this.store.read('chat-bridge/input', request.inputId);
    this.main.admission.authorize(caller, request.expectedBridge, input.value.identity.channel);
    await this.native.verifyOwner(); const current = await this.find(request.inputId);
    requireThat(current, 'not_found', 'The original collection is unavailable.');
    const retryAuthorization = { callerPrincipalId: caller, operationId: request.operationId, reason: request.reason, failed: request.expectedCollection };
    if (same(current.value.retryAuthorization ?? null, retryAuthorization)) return current.value;
    const source = await this.source(request.inputId); await this.check(current, source);
    requireThat(same(current.pin, request.expectedCollection), 'revision_conflict', 'Retry requires the exact displayed failed collection.');
    const status = await this.native.ready(); return (await this.restart(current, status.epoch!, retryAuthorization)).value;
  }
}
