import { canonical, hashJson } from '../../../packages/sdk/src/node.js';
import { IvyError, requireThat } from '../../../packages/sdk/src/node.js';
import type { Chat, Wire } from '../../../packages/sdk/src/node.js';
import { ChatCollector } from './collector.js';
import { ChatCancellation } from './cancellation.js';
import { ChatNotices } from './notices.js';
import { contention } from './operations.js';
import { mutation } from './store.js';
import type { Document } from './store.js';
import { deriveOperationId } from '../../../packages/sdk/src/node.js';

const same = (a: unknown, b: unknown) => canonical(a) === canonical(b);
type Input = Document<'chat-bridge/input'>;
type Call = Document<'chat-bridge/native-call'>;
type Slot = keyof Chat.Input['nativeCalls'];

/** Prevent prepared originals, reconcile already-started originals, interrupt only their turn. */
export class ChatCancelWorker {
  readonly cancellation: ChatCancellation;
  constructor(readonly collector: ChatCollector) { this.cancellation = new ChatCancellation(collector.main); }
  get main() { return this.collector.main; }
  get native() { return this.main.native; }
  get store() { return this.main.store; }
  private async guard(input: Input): Promise<void> {
    const current = await this.collector.delivery.head(input.pin.objectId);
    requireThat(current && same(current.input.pin, input.pin) && current.input.value.cancellation && !current.input.value.finishedAt && !current.input.value.result,
      'revision_conflict', 'Native cancellation requires its unchanged original queue head and retained intent.');
  }
  private async update(input: Input, patch: Partial<Chat.Input>): Promise<void> {
    const next = { ...input.value, ...patch }; if (same(next, input.value)) return;
    await this.guard(input); await this.native.verifyOwner(); next.updatedAt = new Date().toISOString();
    await this.store.write('chat-bridge/input', next, mutation(input.pin.objectId, 'cancel-state:' + input.pin.revision + ':' + hashJson(next)),
      { objectId: input.pin.objectId, expectedRevision: input.pin.revision });
  }
  private async attach(input: Input, slot: Slot, call: Call): Promise<void> {
    const unknown = call.value.state === 'outcome_unknown';
    const patch: Partial<Chat.Input> = { nativeCalls: { ...input.value.nativeCalls, [slot]: call.pin }, state: unknown ? 'outcome_unknown' : 'cancel_requested',
      error: unknown ? { code: call.value.code!, outcome: 'unknown', detail: 'The original native cancellation call has an unknown outcome.' } : null };
    if (slot === 'turn' && call.value.state === 'succeeded') {
      const original = await this.native.evidence.operation(input.pin.objectId, call.value, call.value.evidence!);
      requireThat(original.reply && 'result' in original.reply, 'chat_native_mismatch', 'Cancellation must reconcile the original delivered turn reply.');
      const turn = (original.reply.result as Record<string, Wire.Json>)['turn'] as Record<string, Wire.Json>;
      requireThat(typeof turn['id'] === 'string' && call.value.epoch, 'chat_native_mismatch', 'The original delivered turn must retain its ID and epoch.');
      patch.turnId = turn['id']; patch.epoch = call.value.epoch;
    }
    if (slot === 'interrupt' && call.value.state === 'failed') patch.error = { code: call.value.code!, outcome: 'unknown', detail: 'The original native interrupt failed; terminal turn evidence is still required.' };
    await this.update(input, patch);
  }
  async step(): Promise<void> {
    let input: Input | null = null;
    try {
      await this.main.recoverPending(); const head = await this.collector.delivery.head(); if (!head) return;
      input = head.input; if (!input.value.cancellation || input.value.finishedAt || input.value.result) return;
      await this.cancellation.recover(input.value, input.pin.objectId);
      if (!input.value.turnId) {
        const calls: Partial<Record<'turn' | 'resume', Call>> = {};
        for (const slot of ['turn', 'resume'] as const) {
          const pin = input.value.nativeCalls[slot]; if (!pin) continue;
          const attached = await this.native.get(input.pin.objectId, pin), request = await this.native.evidence.request(input.pin.objectId, attached.value);
          const expectedId = deriveOperationId(input.value.operationId, slot === 'turn' ? 'chat:native:turn/start' : 'chat:native:resume:' + hashJson({ predecessor: attached.value.predecessor, preparedEpoch: attached.value.preparedEpoch }));
          requireThat(attached.value.method === (slot === 'turn' ? 'turn/start' : 'thread/resume') && attached.value.operationId === expectedId &&
            (request.params as Record<string, Wire.Json>)['threadId'] === head.binding.value.primary.nativeId,
            'chat_native_mismatch', 'Prevention must use the original input\'s exact resume or turn call.');
          const call = await this.native.prevent(input.pin.objectId, pin, () => this.guard(input!)); calls[slot] = call;
          if (!same(pin, call.pin) || slot === 'turn' && call.value.state === 'succeeded' || call.value.state === 'outcome_unknown' && input.value.state !== 'outcome_unknown') {
            await this.attach(input, slot, call); return;
          }
          if (!['failed', 'succeeded'].includes(call.value.state)) return;
        }
        const failure = [calls.turn, calls.resume].find(call => call?.value.state === 'failed' && call.value.code !== 'request_prevented');
        await this.update(input, { state: failure ? 'failed' : 'cancelled', finishedAt: new Date().toISOString(),
          error: failure ? { code: failure.value.code!, outcome: failure.value.epoch === null ? 'not_executed' : 'unknown', detail: 'The original native call failed before a turn was delivered.' } : null }); return;
      }
      if (await this.collector.observedTerminal(input.pin.objectId)) return;
      const pin = input.value.nativeCalls.interrupt;
      if (!pin) {
        const plan = head.binding.value.nativePlan, request: Chat.NativeRequest = { nativeVersion: plan.nativeVersion, catalogSourceHash: plan.catalogSourceHash,
          method: 'turn/interrupt', params: { threadId: head.binding.value.primary.nativeId, turnId: input.value.turnId } } as Chat.NativeRequest;
        const call = await this.native.prepare(input.pin.objectId, deriveOperationId(input.value.cancellation!.operationId, 'chat:native:turn/interrupt'), request);
        await this.attach(input, 'interrupt', call); return;
      }
      const call = await this.native.get(input.pin.objectId, pin), request = await this.native.evidence.request(input.pin.objectId, call.value);
      requireThat(call.value.operationId === deriveOperationId(input.value.cancellation!.operationId, 'chat:native:turn/interrupt') && call.value.method === 'turn/interrupt' &&
        same(request.params, { threadId: head.binding.value.primary.nativeId, turnId: input.value.turnId }),
        'chat_native_mismatch', 'Cancellation must retain one interrupt of its exact original primary and turn.');
      await this.attach(input, 'interrupt', await this.native.advance(input.pin.objectId, pin, () => this.guard(input!)));
    } catch (error) {
      if (contention(error)) return;
      if (input && error instanceof IvyError && error.code === 'chat_native_prevention_unknown') {
        await this.update(input, { state: 'outcome_unknown', error: { code: error.code, outcome: 'unknown', detail: error.message } }); return;
      }
      throw error;
    }
  }
  /** Pre-turn failures/cancellations have no native Result. Unknown original calls block release. */
  async settle(): Promise<void> {
    try {
      const head = await this.collector.delivery.head(); if (!head) return;
      const input = head.input;
      if (!input.value.finishedAt || !['failed', 'cancelled'].includes(input.value.state) || input.value.turnId || input.value.result) return;
      if (input.value.cancellation) await this.cancellation.recover(input.value, input.pin.objectId);
      for (const slot of ['turn', 'resume', 'interrupt'] as const) {
        const pin = input.value.nativeCalls[slot]; if (!pin) continue;
        const call = await this.native.get(input.pin.objectId, pin);
        requireThat(slot !== 'interrupt' && call.value.method === (slot === 'turn' ? 'turn/start' : 'thread/resume'), 'chat_native_mismatch', 'An undelivered input cannot have another native call.');
        if (slot === 'turn' ? call.value.state !== 'failed' : !['failed', 'succeeded'].includes(call.value.state)) return;
      }
      const current = await this.main.queue.main(), latest = await this.collector.delivery.head(input.pin.objectId);
      requireThat(!current.value.pendingAction && !current.value.publication && current.value.queue[0]?.inputId === input.pin.objectId && latest && same(latest.input.pin, input.pin),
        'revision_conflict', 'Only the unchanged settled original input can release its ticket.');
      await this.native.verifyOwner();
      await new ChatNotices(this.main).finishUndelivered(input);
      const next: Chat.Main = { ...current.value, queue: current.value.queue.slice(1), updatedAt: input.value.finishedAt };
      await this.store.write('chat-bridge/main', next, mutation(input.pin.objectId, 'settle:' + current.pin.revision + ':' + hashJson(next)),
        { objectId: current.pin.objectId, expectedRevision: current.pin.revision });
    } catch (error) { if (!contention(error)) throw error; }
  }
}
