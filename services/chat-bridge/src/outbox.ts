import { canonical, hashJson } from '../../../packages/sdk/src/node.js';
import { IvyError, requireThat } from '../../../packages/sdk/src/node.js';
import { validateChat } from '../../../packages/sdk/src/node.js';
import type { Chat, Wire } from '../../../packages/sdk/src/node.js';
import { sameChannel } from './admission.js';
import { ChatOperations, contention } from './operations.js';
import { mutation } from './store.js';
import type { Document } from './store.js';
import { ChatQueue, mainName } from './queue.js';

const same = (a: unknown, b: unknown) => canonical(a) === canonical(b);
const bytes = (value: unknown) => Buffer.byteLength(canonical(value));
const maximumPageBytes = 1024 * 1024;
const offerName = (operationId: string) => 'Chat offer ' + operationId;
const acknowledgementName = (operationId: string) => 'Chat acknowledgement ' + operationId;
const immutableReply = ({ state: _state, firstOfferedAt: _first, confirmedAt: _confirmed, immutableHash: _hash, ...value }: Chat.Reply) => value;
type ReplyDocument = Document<'chat-bridge/reply'>;
type OfferDocument = Document<'chat-bridge/offer'>;
type EntryDocument = Document<'chat-bridge/input'> | ReplyDocument;
const view = <K extends 'chat-bridge/input' | 'chat-bridge/reply'>(document: Document<K>) => ({ object: document.pin, data: document.value });

/** Saved reply delivery only. An offer is uncertainty; confirmation requires its authenticated acknowledgement. */
export class ChatOutbox {
  constructor(readonly operations: ChatOperations) {}
  get store() { return this.operations.store; }
  get admission() { return this.operations.admission; }
  private scope(value: Chat.Input | Chat.Reply, channel: Chat.Channel, definitionHash = this.admission.definitionHash): void {
    requireThat(value.workspaceId === this.admission.definition.workspaceId && value.definitionHash === definitionHash &&
      sameChannel('identity' in value ? value.identity.channel : value.channel, channel),
      'chat_scope_mismatch', 'Chat history and replies must retain their exact original workspace, definition and channel.');
  }
  private async entries(key: 'chat-bridge/input' | 'chat-bridge/reply', channel: Chat.Channel, afterSequence: number, limit: number, throughSequence: number) {
    const path = key === 'chat-bridge/input' ? 'identity/channel' : 'channel';
    const root: Wire.QueryPredicate = this.store.rootObjectId === null ? { op: 'isNull', field: 'object.parentId' } : { op: 'eq', field: 'object.parentId', value: this.store.rootObjectId };
    requireThat(this.admission.definition.channels.some(value => sameChannel(value.channel, channel)),
      'chat_scope_mismatch', 'Chat history requires its configured channel.');
    const page = await this.store.client.request('objects.query', { contractKey: key,
      where: { op: 'and', args: [root, { op: 'eq', field: 'data:/workspaceId', value: this.admission.definition.workspaceId },
        { op: 'eq', field: 'data:/definitionHash', value: this.admission.definitionHash },
        ...Object.entries(channel).map(([name, value]): Wire.QueryPredicate => ({ op: 'eq', field: 'data:/' + path + '/' + name, value })),
        { op: 'gt', field: 'data:/sequence', value: afterSequence }, { op: 'lte', field: 'data:/sequence', value: throughSequence }] },
      orderBy: [{ field: 'data:/sequence', direction: 'asc' }], limit: limit + 1 });
    const entries: EntryDocument[] = [];
    for (const item of page.items) {
      const document = key === 'chat-bridge/reply' ? await this.reply({ objectId: item.objectId, revision: item.revision }, channel)
        : await this.store.read('chat-bridge/input', { objectId: item.objectId, revision: item.revision });
      this.scope(document.value, channel);
      entries.push(document);
    }
    requireThat(entries.every((entry, index) => entry.value.sequence > afterSequence && (index === 0 || entry.value.sequence > entries[index - 1]!.value.sequence)),
      'chat_identity_conflict', 'Channel history cannot contain duplicate or nonadvancing sequences.');
    return { entries, hasMore: page.nextCursor !== null };
  }
  private async visible(keys: Array<'chat-bridge/input' | 'chat-bridge/reply'>, channel: Chat.Channel, afterSequence: number, limit: number) {
    const boundary = async () => await this.store.named('chat-bridge/main', mainName(this.admission.definition)) ? new ChatQueue(this.operations).main() : null;
    for (let attempt = 0; attempt < 8; attempt++) {
      const before = await boundary();
      const pending = before?.value.pendingAction ? await this.store.read('chat-bridge/operation', before.value.pendingAction) : null;
      if (pending) this.operations.check(pending.value);
      const through = Math.min(before?.value.publication ? before.value.publication.firstSequence - 1 : Number.MAX_SAFE_INTEGER,
        pending?.value.request.action === 'notify' ? before!.value.nextSequence - 1 : Number.MAX_SAFE_INTEGER);
      const pages = await Promise.all(keys.map(key => this.entries(key, channel, afterSequence, limit, through)));
      const after = await boundary();
      if (same(before?.pin ?? null, after?.pin ?? null)) return pages;
    }
    throw new IvyError('chat_contention', 'History publication changed repeatedly. Retry this same bounded query.', 'unknown');
  }
  async reply(reference: Chat.ObjectPin | string, channel: Chat.Channel): Promise<ReplyDocument> {
    const selected = await this.store.read('chat-bridge/reply', reference); this.scope(selected.value, channel, selected.value.definitionHash);
    requireThat(selected.value.immutableHash === hashJson(immutableReply(selected.value)),
      'chat_identity_conflict', 'A saved reply cannot change its original content, sequence, input, binding or result.');
    const origin = selected.value.origin;
    if (origin.kind === 'notice') {
      requireThat(origin.producerPrincipalId.length > 0 && origin.source.revision > 0 && sameChannel(channel, selected.value.channel) &&
        selected.metadata.name === 'Chat notice ' + origin.operation.objectId && selected.value.partIndex === 0 && selected.value.artifacts.length === 0,
        'chat_identity_conflict', 'A final notice must retain its admitted producer, source, channel and immutable content without its expired delivery receipt.');
      return selected;
    }
    const input = await this.store.read('chat-bridge/input', origin.inputId); this.scope(input.value, channel, selected.value.definitionHash);
    const result = await this.store.read('chat-bridge/result', origin.result, input.pin.objectId);
    requireThat(['completed', 'failed', 'cancelled'].includes(input.value.state) && same(input.value.result, origin.result) && same(origin.binding, input.value.binding) &&
      result.value.inputId === input.pin.objectId && same(result.value.binding, origin.binding) &&
      result.value.turnId === input.value.turnId && selected.value.sequence > input.value.sequence,
      'chat_identity_conflict', 'Reply delivery needs its original terminal input and immutable saved result.');
    return selected;
  }
  /** Pending service replies survive configuration partitions; their exact input/result is still verified. */
  async pendingNotices(cursor?: string): Promise<{ replies: ReplyDocument[]; nextCursor: string | null }> {
    const root: Wire.QueryPredicate = this.store.rootObjectId === null ? { op: 'isNull', field: 'object.parentId' }
      : { op: 'eq', field: 'object.parentId', value: this.store.rootObjectId };
    const page = await this.store.client.request('objects.query', { contractKey: 'chat-bridge/reply',
      where: { op: 'and', args: [root, { op: 'eq', field: 'data:/workspaceId', value: this.admission.definition.workspaceId },
        { op: 'eq', field: 'data:/partIndex', value: 0 },
        { op: 'ne', field: 'data:/state', value: 'confirmed' }] },
      orderBy: [{ field: 'object.createdAt', direction: 'asc' }], limit: 32, ...(cursor ? { cursor } : {}) });
    const replies: ReplyDocument[] = [];
    for (const item of page.items) {
      const saved = await this.store.read('chat-bridge/reply', { objectId: item.objectId, revision: item.revision });
      if (saved.value.origin.kind !== 'native') continue;
      const input = await this.store.read('chat-bridge/input', saved.value.origin.inputId);
      const operation = await this.operations.find(input.value.identity.senderPrincipalId, input.value.operationId);
      if (operation?.value.request.action !== 'notify') continue;
      requireThat(operation.value.phase === 'succeeded' && operation.value.outcome?.action === 'notify' &&
        operation.value.outcome.reply.object.objectId === saved.pin.objectId &&
        operation.value.definitionHash === saved.value.definitionHash && operation.value.workspaceId === saved.value.workspaceId &&
        same(operation.value.request.channel, saved.value.channel),
        'chat_identity_conflict', 'A service reply requires its original completed producer operation.');
      replies.push(await this.reply(saved.pin, saved.value.channel));
    }
    return { replies, nextCursor: page.nextCursor };
  }
  async noticeParts(first: ReplyDocument): Promise<ReplyDocument[]> {
    requireThat(first.value.origin.kind === 'native' && first.value.partIndex === 0, 'chat_identity_conflict', 'A notice must begin with its original first reply.');
    const parts: ReplyDocument[] = [];
    let cursor: string | undefined;
    do {
      const page = await this.store.client.request('objects.query', { contractKey: 'chat-bridge/reply',
        where: { op: 'and', args: [this.store.rootObjectId === null ? { op: 'isNull', field: 'object.parentId' }
          : { op: 'eq', field: 'object.parentId', value: this.store.rootObjectId },
          { op: 'eq', field: 'data:/workspaceId', value: first.value.workspaceId },
          { op: 'eq', field: 'data:/origin/inputId', value: first.value.origin.inputId }] },
        orderBy: [{ field: 'data:/partIndex', direction: 'asc' }], limit: 32, ...(cursor ? { cursor } : {}) });
      for (const item of page.items) {
        const part = await this.reply({ objectId: item.objectId, revision: item.revision }, first.value.channel);
        requireThat(part.value.partIndex === parts.length && same(part.value.origin, first.value.origin) &&
          part.value.definitionHash === first.value.definitionHash && part.value.sequence === first.value.sequence + parts.length,
          'chat_identity_conflict', 'Notice delivery must retain every ordered part of the same Main answer.');
        parts.push(part);
      }
      cursor = page.nextCursor ?? undefined;
    } while (cursor);
    requireThat(parts[0]?.pin.objectId === first.pin.objectId, 'chat_identity_conflict', 'The exact first notice reply is unavailable.');
    return parts;
  }
  /** Internal transport receipt: called only after the durable WhatsApp journal confirms every part. */
  async confirmNotice(parts: ReplyDocument[]): Promise<void> {
    requireThat(parts.length > 0 && parts[0]!.value.partIndex === 0, 'chat_identity_conflict', 'Notice confirmation requires its ordered reply parts.');
    // The producer reads part zero. Confirm it last, so a crash cannot acknowledge a partial answer.
    for (const reply of [...parts.slice(1), parts[0]!])
      await this.mark(reply, 'confirmed', new Date().toISOString(), 'whatsapp-notice:' + reply.pin.objectId);
  }
  async history(caller: string, request: Chat.HistoryQuery): Promise<Chat.HistoryPage> {
    this.admission.authorize(caller, request.expectedBridge, request.channel); validateChat('HistoryQuery', request);
    const original = structuredClone(request);
    const pages = await this.visible(['chat-bridge/input', 'chat-bridge/reply'], original.channel, original.afterSequence, original.limit);
    const inputs = pages[0]!, replies = pages[1]!;
    const candidates = [...inputs.entries, ...replies.entries].sort((a, b) => a.value.sequence - b.value.sequence);
    const through = Math.min(...pages.map(page => page.hasMore ? page.entries.at(-1)!.value.sequence : Number.MAX_SAFE_INTEGER));
    requireThat(new Set(candidates.map(entry => entry.value.sequence)).size === candidates.length, 'chat_identity_conflict', 'Inputs and replies share one workspace sequence.');
    const page: Chat.HistoryPage = { entries: [], throughSequence: original.afterSequence, hasMore: inputs.hasMore || replies.hasMore };
    for (const candidate of candidates) {
      if (candidate.value.sequence > through) { page.hasMore = true; break; }
      if (page.entries.length === original.limit) { page.hasMore = true; break; }
      // Hidden service inputs still consume sequence positions. Advance past them only
      // in the merged order, so neither an empty page nor a later reply can hide history.
      if (candidate.key === 'chat-bridge/input' && candidate.value.identity.messageId === candidate.value.operationId) {
        page.throughSequence = candidate.value.sequence;
        continue;
      }
      const entry: Chat.InputView | Chat.ReplyView = candidate.key === 'chat-bridge/input' ? { object: candidate.pin, data: candidate.value } : { object: candidate.pin, data: candidate.value };
      const next: Chat.HistoryPage = { entries: [...page.entries, entry], throughSequence: candidate.value.sequence, hasMore: false };
      if (bytes(next) > maximumPageBytes) {
        requireThat(page.entries.length > 0, 'chat_page_too_large', 'The next history entry exceeds the complete page byte limit.'); page.hasMore = true; break;
      }
      page.entries = next.entries; page.throughSequence = next.throughSequence;
    }
    validateChat('HistoryPage', page); return page;
  }
  private async offer(reference: Chat.ObjectPin | string, caller: string): Promise<{ document: OfferDocument; replies: ReplyDocument[] }> {
    const document = await this.store.read('chat-bridge/offer', reference), value = document.value;
    requireThat(document.pin.revision === 1 && value.callerPrincipalId === caller, 'chat_offer_mismatch', 'Only the original caller can use an immutable offer.');
    this.admission.authorize(caller, this.admission.expected(caller), value.channel);
    const operation = await this.operations.find(caller, value.operationId);
    requireThat(operation?.value.request.action === 'receive' && document.metadata.name === offerName(operation.pin.objectId) &&
      sameChannel(value.channel, operation.value.request.channel) && value.afterSequence === operation.value.request.afterSequence &&
      value.createdAt === operation.value.createdAt && value.replies.length <= operation.value.request.limit &&
      new Set(value.replies.map(pin => pin.objectId)).size === value.replies.length,
      'chat_offer_mismatch', 'An offer must retain its exact receiving operation, channel and selected reply identities.');
    const replies: ReplyDocument[] = [];
    for (const pin of value.replies) replies.push(await this.reply(pin, value.channel));
    requireThat(replies.every((reply, index) => reply.value.sequence > value.afterSequence &&
      (index === 0 || reply.value.sequence > replies[index - 1]!.value.sequence)) && value.throughSequence === (replies.at(-1)?.value.sequence ?? value.afterSequence),
      'chat_offer_mismatch', 'An offer cursor cannot skip or reorder its exact selected replies.');
    return { document, replies };
  }
  private async rememberConfirmedNotice(reply: ReplyDocument): Promise<void> {
    if (reply.value.state !== 'confirmed' || reply.value.partIndex !== 0 || reply.value.origin.kind !== 'native') return;
    const input = await this.store.read('chat-bridge/input', reply.value.origin.inputId);
    const operation = await this.operations.find(input.value.identity.senderPrincipalId, input.value.operationId);
    if (operation?.value.request.action === 'notify') this.store.rememberNoticeDelivery(reply.pin.objectId);
  }
  private async mark(original: ReplyDocument, state: 'outcome_unknown' | 'confirmed', at: string, scope: string): Promise<void> {
    for (let attempt = 0; attempt < 16; attempt++) {
      const current = await this.reply(original.pin.objectId, original.value.channel);
      requireThat(same(immutableReply(current.value), immutableReply(original.value)) &&
        (original.value.state !== 'confirmed' || current.value.state === 'confirmed') &&
        (original.value.state === 'pending' || current.value.state !== 'pending'),
        'chat_identity_conflict', 'The original reply content and confirmed delivery cannot regress.');
      if (current.value.state === 'confirmed' || current.value.state === state) {
        await this.rememberConfirmedNotice(current); return;
      }
      const next: Chat.Reply = { ...current.value, state, firstOfferedAt: current.value.firstOfferedAt ?? at, confirmedAt: state === 'confirmed' ? at : null };
      try {
        const pin = await this.store.write('chat-bridge/reply', next, mutation(scope, state + ':' + current.pin.objectId + ':' + current.pin.revision + ':' + hashJson(next)),
          { objectId: current.pin.objectId, expectedRevision: current.pin.revision });
        await this.rememberConfirmedNotice({ ...current, pin, value: next }); return;
      } catch (error) { if (!contention(error)) throw error; }
    }
    throw new IvyError('chat_contention', 'Recover the original offer or acknowledgement after concurrent reply updates.', 'unknown');
  }
  async receive(caller: string, original: Chat.ReceiveRequest): Promise<Chat.Operation> {
    this.admission.authorize(caller, original.expectedBridge, original.channel); validateChat('ReceiveRequest', original);
    const request = structuredClone(original), operation = await this.operations.begin(caller, request), scope = operation.pin.objectId;
    if (operation.value.phase === 'succeeded' || operation.value.phase === 'failed') return operation.value;
    const name = offerName(scope);
    let saved = await this.store.named('chat-bridge/offer', name);
    if (!saved) {
      const page = (await this.visible(['chat-bridge/reply'], request.channel, request.afterSequence, request.limit))[0]!;
      const selected: ReplyDocument[] = [], offer: Chat.Offer = { schemaVersion: 1, operationId: request.operationId, callerPrincipalId: caller,
        channel: request.channel, afterSequence: request.afterSequence, throughSequence: request.afterSequence, replies: [], createdAt: operation.value.createdAt };
      // Hive creates UUID Object IDs. Reserve that complete pin and the full Operation envelope,
      // including the caller's original request, before selecting another reply.
      const reservedPin: Chat.ObjectPin = { objectId: '00000000-0000-0000-0000-000000000000', revision: 1 };
      for (const candidate of page.entries.slice(0, request.limit) as ReplyDocument[]) {
        const next = { ...offer, throughSequence: candidate.value.sequence, replies: [...offer.replies, candidate.pin] };
        const outcome: Chat.ActionOutcome = { action: 'receive', operationId: request.operationId, offer: { object: reservedPin, data: next }, replies: [...selected, candidate].map(view) };
        if (bytes({ ...operation.value, phase: 'succeeded', outcome }) > maximumPageBytes) {
          requireThat(selected.length > 0, 'chat_page_too_large', 'The next reply exceeds the complete receive response byte limit.'); break;
        }
        selected.push(candidate); offer.replies = next.replies; offer.throughSequence = next.throughSequence;
      }
      try {
        const pin = await this.store.write('chat-bridge/offer', offer, mutation(scope, 'publish-offer'), { create: { parentId: this.store.rootObjectId, name } });
        saved = await this.store.read('chat-bridge/offer', pin);
      } catch (error) {
        if (!contention(error)) throw error; saved = await this.store.named('chat-bridge/offer', name);
        requireThat(saved, 'chat_offer_mismatch', 'The competing original offer must remain discoverable.');
      }
    }
    const offered = await this.offer(saved.pin, caller);
    for (const reply of offered.replies) await this.mark(reply, 'outcome_unknown', offered.document.value.createdAt, scope);
    const outcome: Chat.ActionOutcome = { action: 'receive', operationId: request.operationId, offer: viewOffer(offered.document), replies: offered.replies.map(view) };
    requireThat(bytes({ ...operation.value, phase: 'succeeded', outcome }) <= maximumPageBytes, 'chat_page_too_large', 'The original complete receive response exceeds its byte limit.');
    return (await this.operations.finish(scope, outcome)).value;
  }
  async acknowledge(caller: string, original: Chat.AcknowledgeRequest): Promise<Chat.Operation> {
    this.admission.authorize(caller, original.expectedBridge); validateChat('AcknowledgeRequest', original);
    const request = structuredClone(original), previous = await this.operations.find(caller, request.operationId);
    if (previous) {
      requireThat(previous.value.requestHash === hashJson(request), 'mutation_conflict', 'This operation already names different ChatBridge arguments.');
      if (previous.value.phase === 'succeeded' || previous.value.phase === 'failed') return previous.value;
    }
    const offered = await this.offer(request.offerId, caller);
    requireThat(request.replyIds.every(id => offered.replies.some(reply => reply.pin.objectId === id)), 'chat_offer_mismatch', 'An acknowledgement may select only replies from this caller\'s exact original offer.');
    const operation = await this.operations.begin(caller, request), scope = operation.pin.objectId;
    if (operation.value.phase === 'succeeded' || operation.value.phase === 'failed') return operation.value;
    const name = acknowledgementName(scope), value: Chat.Acknowledgement = { schemaVersion: 1, operationId: request.operationId, callerPrincipalId: caller,
      offer: offered.document.pin, replyIds: request.replyIds, createdAt: operation.value.createdAt };
    let saved = await this.store.named('chat-bridge/acknowledgement', name);
    if (!saved) {
      try {
        const pin = await this.store.write('chat-bridge/acknowledgement', value, mutation(scope, 'publish-acknowledgement'), { create: { parentId: this.store.rootObjectId, name } });
        saved = await this.store.read('chat-bridge/acknowledgement', pin);
      } catch (error) {
        if (!contention(error)) throw error; saved = await this.store.named('chat-bridge/acknowledgement', name);
        requireThat(saved, 'chat_offer_mismatch', 'The original acknowledgement must remain discoverable.');
      }
    }
    requireThat(saved.pin.revision === 1 && same(saved.value, value), 'chat_offer_mismatch', 'An acknowledgement cannot change its original offer, caller or selected reply identities.');
    for (const reply of offered.replies.filter(reply => request.replyIds.includes(reply.pin.objectId))) await this.mark(reply, 'confirmed', saved.value.createdAt, scope);
    return (await this.operations.finish(scope, { action: 'acknowledge', operationId: request.operationId, acknowledgement: { object: saved.pin, data: saved.value } })).value;
  }
}
const viewOffer = (document: OfferDocument): Chat.OfferView => ({ object: document.pin, data: document.value });
