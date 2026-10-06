import { canonical, nativeInstant as instant, nativeTurnSnapshot, readNativeTurnContents, verifyNativeTurnContents } from '../../../packages/sdk/src/node.js';
import type { Agent, NativeTurnContents } from '../../../packages/sdk/src/node.js';
import { record } from './native-record.js';
import { same, validate } from './schema.js';
import type { Assessment, Pin } from './schema.js';
import type { TriageNative, TriageCallDocument, TriagePlanDocument } from './triage-native.js';
import { triageName, triageCheck as check } from './triage-store.js';
import type { TriageDocument } from './triage-store.js';
type Completion = TriageDocument<'secretary/triage-completion'>;
const readParams = (threadId: string, full: boolean) => ({ threadId, cursor: null, limit: 1, sortDirection: 'desc', itemsView: full ? 'full' : 'notLoaded' });
const readName = (plan: Pin, turnId: string, epoch: string, full: boolean) => triageName('read', [plan, turnId, epoch, full]);
export function triageTurn(read: Agent.ReadObservation, turnId: string, full: boolean) {
  check('result' in read.reply); const value = record('result' in read.reply ? read.reply.result : null), data = value['data'];
  check(Array.isArray(data) && data.length === 1, 'secretary_triage_turn_changed');
  const turn = record((data as unknown[])[0]); check(turn['id'] === turnId && turn['itemsView'] === (full ? 'full' : 'notLoaded'), 'secretary_triage_turn_changed'); return turn;
}
export function triageResult(read: Agent.ReadObservation, turnId: string): Assessment {
  const turn = triageTurn(read, turnId, true); check(Array.isArray(turn['items']), 'secretary_triage_turn_failed');
  return resultItems(turn, turn['items'] as unknown[]);
}
function resultItems(turn: Record<string, unknown>, items: unknown[]): Assessment {
  check(turn['status'] === 'completed' && turn['error'] == null, 'secretary_triage_turn_failed');
  const ids = new Set<string>(), finals: string[] = [];
  for (const raw of items) {
    const item = record(raw); check(typeof item['id'] === 'string' && item['id'] && !ids.has(item['id']), 'secretary_triage_result_invalid'); ids.add(item['id'] as string);
    check(['userMessage', 'agentMessage', 'reasoning'].includes(String(item['type'])), 'secretary_triage_unexpected_action');
    if (item['type'] === 'agentMessage') {
      check(typeof item['text'] === 'string', 'secretary_triage_result_invalid');
      if (item['phase'] === 'final_answer') finals.push(item['text'] as string);
    }
  }
  check(finals.length === 1, 'secretary_triage_final_ambiguous'); let result: unknown;
  try { result = JSON.parse(finals[0]!); validate('Assessment', result); } catch { check(false, 'secretary_triage_result_invalid'); }
  // This prompt supplies no TaskBoard task authority; a model cannot invent a task link.
  check((result as Assessment).task === null && (result as Assessment).disposition !== 'task', 'secretary_triage_result_invalid');
  return result as Assessment;
}
export class TriageResults {
  constructor(readonly native: TriageNative, readonly plan: TriagePlanDocument) {}
  private get store() { return this.native.store; }
  private get owner() { return this.native.owner(this.plan); }
  // A media turn may echo its complete 6MiB input. Text-only work retains the existing 2MiB bound.
  private get maximumReadBytes() { return this.plan.value.input.media ? 8 * 1024 * 1024 : 2 * 1024 * 1024; }
  private decode(bytes: Buffer): NativeTurnContents {
    return JSON.parse(new TextDecoder('utf-8', { fatal: true }).decode(bytes)) as NativeTurnContents;
  }
  private turnId(turn: TriageCallDocument, threadId: string): string {
    check(same(turn.value.plan, this.plan.pin) && turn.value.method === 'turn/start' && turn.value.params['threadId'] === threadId && turn.value.observation?.phase === 'succeeded');
    this.owner.checkOperation(turn.value.observation, turn.value); const reply = turn.value.observation!.reply; check(reply && 'result' in reply);
    const value = record(record(reply && 'result' in reply ? reply.result : null)['turn']); check(typeof value['id'] === 'string' && value['id'], 'secretary_triage_turn_changed'); return value['id'] as string;
  }
  private pair(contents: NativeTurnContents, verification: Agent.ReadObservation, threadId: string, turnId: string): void {
    const snapshot = nativeTurnSnapshot(contents), last = 'format' in contents ? contents.pages.at(-1)! : snapshot;
    this.owner.checkRead(verification, 'thread/turns/list', readParams(threadId, false), snapshot.epoch);
    check(verification.observationId !== last.observationId && instant(verification.observedAt) >= instant(last.observedAt) &&
      triageTurn(verification, turnId, false)['status'] === 'completed' && triageTurn(verification, turnId, false)['error'] == null, 'secretary_triage_verification_changed');
  }
  async find(): Promise<Completion | null> {
    return this.store.find('secretary/triage-completion', triageName('completion', this.plan.pin), true);
  }
  async collect(turn: TriageCallDocument, threadId: string, epoch: string): Promise<Completion | null> {
    const turnId = this.turnId(turn, threadId), name = readName(this.plan.pin, turnId, epoch, true);
    let saved = await this.store.binary(name, undefined, this.maximumReadBytes), full: NativeTurnContents;
    if (saved) full = this.decode(saved.bytes);
    else {
      const snapshot = await this.owner.read('thread/turns/list', readParams(threadId, false), epoch);
      const state = triageTurn(snapshot, turnId, false)['status']; if (state === 'inProgress') return null;
      check(['completed', 'failed', 'interrupted'].includes(String(state)), 'secretary_triage_turn_changed');
      full = await readNativeTurnContents(this.owner, snapshot, threadId, turnId, this.maximumReadBytes);
      await this.native.guard(this.plan); const bytes = Buffer.from(canonical(full, this.maximumReadBytes));
      saved = { pin: await this.store.saveBinary(name, bytes, this.maximumReadBytes), bytes };
    }
    this.owner.checkRead(nativeTurnSnapshot(full), 'thread/turns/list', readParams(threadId, !('format' in full)), epoch);
    const verified = await verifyNativeTurnContents(this.owner, full, threadId, turnId);
    const result = resultItems(verified.turn, verified.items), verifyName = readName(this.plan.pin, turnId, epoch, false);
    let verifySaved = await this.store.binary(verifyName, undefined, this.maximumReadBytes);
    const verification = verifySaved ? this.decode(verifySaved.bytes) as Agent.ReadObservation : await this.owner.read('thread/turns/list', readParams(threadId, false), epoch);
    this.pair(full, verification, threadId, turnId);
    if (!verifySaved) { await this.native.guard(this.plan); const bytes = Buffer.from(canonical(verification, this.maximumReadBytes)); verifySaved = { pin: await this.store.saveBinary(verifyName, bytes, this.maximumReadBytes), bytes }; }
    await this.native.guard(this.plan);
    const completion = await this.store.create('secretary/triage-completion', triageName('completion', this.plan.pin), { schemaVersion: 1, plan: this.plan.pin,
      turn: turn.pin, threadId, turnId, full: saved.pin, verification: verifySaved.pin, result, completedAt: verification.observedAt }, true);
    return completion;
  }
}
