import { canonical, hashJson } from '../../../../packages/sdk/src/node.js';
import { requireThat } from '../../../../packages/sdk/src/node.js';
import { validateAgent } from '../../../../packages/sdk/src/node.js';
import { validateTaskBoard } from '../../../../packages/sdk/src/node.js';
import type { Agent, TaskBoard, Wire } from '../../../../packages/sdk/src/node.js';
import { nativeTurnItems } from '../../../../packages/sdk/src/native-observations.js';
import { catalogFor } from './evidence.js';
import { same } from './checks.js';
import { nativeArray, nativeRecord, nativeString } from './native-intent.js';
import { TaskBoardReadEvidence } from './read-evidence.js';
import { evidenceMaximumBytes } from './evidence-bytes.js';
import { mutation, TaskBoardStore } from './store.js';
import type { Document } from './store.js';

const terminal = (status: string) => ['completed', 'failed', 'interrupted'].includes(status);
const artifactOf = <K extends 'task-board/native-turn-snapshot' | 'task-board/native-transcript'>(document: Document<K>): TaskBoard.Artifact => ({
  object: document.pin, label: document.metadata.name, contentHash: document.revision.contentHash, mediaType: 'application/json',
});
export const pageCursor = (value: Wire.Json | undefined): string | null => {
  requireThat(value === undefined || value === null || typeof value === 'string' && value.length > 0 && value.length <= 8192,
    'task_board_native_cursor_unsupported', 'Native cursor must fit its explicit nonempty bounded opaque representation.'); return value ?? null;
};
export const turnsParams = (threadId: string, cursor: string | null) => ({ threadId, cursor, limit: 100, itemsView: 'notLoaded', sortDirection: 'desc' } as const);
export const outputParams = (threadId: string, turnId: string, cursor: string | null) => ({ threadId, turnId, cursor, limit: 1, sortDirection: 'desc' } as const);
export const finalOutputText = (item: Record<string, Wire.Json>): string =>
  item['type'] === 'agentMessage' && item['phase'] !== 'commentary' ? nativeString(item['text']) : '';
export function boundedOutputSummary(text: string): string {
  if (Buffer.byteLength(text) <= 262144) return text;
  let shortened = text.slice(0, 65536); if (/[\uD800-\uDBFF]$/.test(shortened)) shortened = shortened.slice(0, -1);
  return shortened + '\n\nThe complete native output remains in the original Codex conversation.';
}

/** Verifies read artifacts without native I/O or accumulating complete output bodies in memory. */
export class TaskBoardTurnEvidence {
  readonly reads: TaskBoardReadEvidence;
  constructor(readonly store: TaskBoardStore, readonly principalId: string) { this.reads = new TaskBoardReadEvidence(store); }
  private async run(ref: TaskBoard.ObjectPin) {
    const run = await this.store.read('task-board/run', ref), plan = run.value.plan;
    requireThat(run.value.primaryResourceRef?.namespace === 'codex' && run.value.primaryResourceRef.kind === 'thread' && run.value.primaryResourceRef.serviceNodeId === run.value.target.serviceNodeId && run.value.turnId && run.value.nativeEpoch,
      'task_board_turn_mismatch', 'Turn evidence requires the saved original primary and turn identity.');
    return { run, nativeVersion: plan.nativeVersion, threadId: run.value.primaryResourceRef.nativeId, turnId: run.value.turnId };
  }
  private expected(run: TaskBoard.Run, nativeVersion: string, ref: Pick<TaskBoard.ReadEvidenceRef, 'observationId' | 'epoch'>, method: string, params: Wire.Json) {
    return { observationId: ref.observationId, callerPrincipalId: this.principalId, serviceNodeId: run.target.serviceNodeId, nativeVersion, epoch: ref.epoch, method, params };
  }
  private selectTurn(reply: Agent.Reply, turnId: string): Record<string, Wire.Json> {
    requireThat('result' in reply, 'task_board_native_evidence_required', 'Native turn inspection requires its successful original reply.');
    const matches = nativeArray(nativeRecord(reply.result)['data']).map(nativeRecord).filter(value => value['id'] === turnId);
    requireThat(matches.length === 1, 'task_board_turn_unavailable', 'The page must contain exactly the original known turn.'); return matches[0]!;
  }
  async saveSnapshot(runPin: TaskBoard.ObjectPin, metadata: Agent.ReadObservation, turns: Agent.ReadObservation): Promise<TaskBoard.Artifact> {
    validateAgent('ReadObservation', metadata); validateAgent('ReadObservation', turns);
    const source = await this.run(runPin), cursor = pageCursor(nativeRecord(turns.params)['cursor']);
    requireThat(metadata.epoch === turns.epoch, 'task_board_native_epoch_changed', 'Metadata and selected turn must be observed in the same native epoch.');
    const selected = this.selectTurn(turns.reply, source.turnId), status = nativeString(selected['status']);
    const readRef = (value: Agent.ReadObservation, artifact: TaskBoard.Artifact): TaskBoard.ReadEvidenceRef => ({ artifact, observationId: value.observationId, epoch: value.epoch });
    const metadataExpected = this.expected(source.run.value, source.nativeVersion, metadata, 'thread/read', { threadId: source.threadId, includeTurns: false });
    const turnsExpected = this.expected(source.run.value, source.nativeVersion, turns, 'thread/turns/list', turnsParams(source.threadId, cursor));
    const metaArtifact = await this.reads.save(runPin, metadata, metadataExpected), turnArtifact = await this.reads.save(runPin, turns, turnsExpected);
    const value: TaskBoard.NativeTurnSnapshot = { schemaVersion: 1, run: runPin, nativeVersion: source.nativeVersion, serviceNodeId: source.run.value.target.serviceNodeId,
      epoch: metadata.epoch, threadId: source.threadId, turnId: source.turnId, turnCursor: cursor, metadata: readRef(metadata, metaArtifact), turn: readRef(turns, turnArtifact),
      status: status as TaskBoard.NativeTurnSnapshot['status'], createdAt: turns.observedAt };
    validateTaskBoard('NativeTurnSnapshot', value);
    const pin = await this.store.write('task-board/native-turn-snapshot', value, mutation(runPin.objectId, 'turn-snapshot:' + hashJson(value)),
      { create: { parentId: runPin.objectId, name: 'Native turn snapshot ' + hashJson(value).slice(7) } });
    const artifact = artifactOf(await this.store.read('task-board/native-turn-snapshot', pin, false));
    await this.snapshot(runPin, artifact); return artifact;
  }
  async snapshot(runPin: TaskBoard.ObjectPin, artifact: TaskBoard.Artifact) {
    validateTaskBoard('Artifact', artifact); const source = await this.run(runPin);
    const document = await this.store.read('task-board/native-turn-snapshot', artifact.object, false), value = document.value;
    requireThat(document.metadata.parentId === runPin.objectId && document.revision.contentHash === artifact.contentHash && artifact.mediaType === 'application/json' &&
      value.run.objectId === runPin.objectId && value.run.revision <= runPin.revision && value.nativeVersion === source.nativeVersion && value.serviceNodeId === source.run.value.target.serviceNodeId &&
      value.threadId === source.threadId && value.turnId === source.turnId && value.metadata.epoch === value.epoch && value.turn.epoch === value.epoch,
      'task_board_evidence_mismatch', 'The snapshot must retain the exact selected Run, owner, version, epoch, thread and turn.');
    const original = await this.run(value.run);
    requireThat(same(original.run.value.originalRequest, source.run.value.originalRequest) && original.threadId === source.threadId && original.turnId === source.turnId,
      'task_board_evidence_mismatch', 'Historical snapshot scope cannot substitute another original execution identity.');
    const metadata = await this.reads.read(value.run, value.metadata.artifact, this.expected(source.run.value, source.nativeVersion, value.metadata, 'thread/read', { threadId: source.threadId, includeTurns: false }));
    const turns = await this.reads.read(value.run, value.turn.artifact, this.expected(source.run.value, source.nativeVersion, value.turn, 'thread/turns/list', turnsParams(source.threadId, value.turnCursor)));
    requireThat('result' in metadata.reply, 'task_board_native_evidence_required', 'Snapshot metadata must retain a successful native reply.');
    const thread = nativeRecord(nativeRecord(metadata.reply.result)['thread']), turn = this.selectTurn(turns.reply, source.turnId);
    requireThat(thread['id'] === source.threadId && thread['ephemeral'] === false && turn['status'] === value.status && value.createdAt === turns.observedAt &&
      Number.isFinite(Date.parse(metadata.observedAt)) && Number.isFinite(Date.parse(turns.observedAt)) && Date.parse(metadata.observedAt) <= Date.parse(turns.observedAt),
      'task_board_evidence_mismatch', 'Snapshot metadata and status must agree with the original native bodies.');
    return { document, source, thread, turn };
  }
  async saveOutput(runPin: TaskBoard.ObjectPin, snapshot: TaskBoard.Artifact, cursor: string | null, observation: Agent.ReadObservation) {
    const source = await this.run(runPin), params = outputParams(source.threadId, source.turnId, cursor);
    const artifact = await this.reads.save(runPin, observation, this.expected(source.run.value, source.nativeVersion, observation, 'thread/items/list', params));
    const output = { cursor, evidence: { artifact, observationId: observation.observationId, epoch: observation.epoch } };
    await this.completion(runPin, snapshot, output); return output;
  }
  async completion(runPin: TaskBoard.ObjectPin, artifact: TaskBoard.Artifact,
    output: Extract<TaskBoard.NativeUpdateRequest['change'], { kind: 'completion' }>['output']) {
    const snapshot = await this.snapshot(runPin, artifact), value = snapshot.document.value, source = snapshot.source;
    requireThat(terminal(value.status), 'task_board_native_state_changed', 'Completion requires the exact terminal native turn.');
    let text = '';
    if (output) {
      requireThat(output.evidence.epoch === value.epoch, 'task_board_native_epoch_changed', 'Final output must come from the terminal snapshot epoch.');
      const observed = await this.reads.read(runPin, output.evidence.artifact, this.expected(source.run.value, source.nativeVersion, output.evidence,
        'thread/items/list', outputParams(source.threadId, source.turnId, output.cursor)));
      requireThat('result' in observed.reply && Number.isFinite(Date.parse(observed.observedAt)) && Date.parse(observed.observedAt) >= Date.parse(value.createdAt),
        'task_board_native_evidence_required', 'Final output requires a successful read following its terminal snapshot.');
      const items = nativeTurnItems(catalogFor(source.nativeVersion, { catalogHash: observed.catalogHash }).contract, observed.reply.result, source.turnId);
      requireThat(items.length === 1 && !!(text = finalOutputText(items[0]!)), 'task_board_native_evidence_required', 'Final output must be one assistant answer from the exact turn.');
    }
    return { snapshot, status: value.status, summary: boundedOutputSummary(text || 'Native turn ' + value.status + '.') };
  }
  async savePage(runPin: TaskBoard.ObjectPin, snapshot: TaskBoard.NativeTurnSnapshot, collectionId: string, previous: TaskBoard.ObjectPin | null,
    index: number, cursor: string | null, observation: Agent.ReadObservation): Promise<TaskBoard.ObjectPin> {
    requireThat(observation.epoch === snapshot.epoch, 'task_board_native_epoch_changed', 'All result pages must come from the collection epoch.');
    const source = await this.run(runPin), params = { threadId: snapshot.threadId, turnId: snapshot.turnId, cursor, limit: 100, sortDirection: 'asc' };
    const artifact = await this.reads.save(runPin, observation, this.expected(source.run.value, source.nativeVersion, observation, 'thread/items/list', params));
    const ref: TaskBoard.ReadEvidenceRef = { observationId: observation.observationId, epoch: observation.epoch, artifact };
    requireThat('result' in observation.reply, 'task_board_native_evidence_required', 'A result page requires a successful native reply.');
    const body = nativeRecord(observation.reply.result);
    const value: TaskBoard.NativeResultPage = { schemaVersion: 1, runId: runPin.objectId, collectionId, previous, index, cursor,
      nextCursor: pageCursor(body['nextCursor']), evidence: ref, itemCount: nativeArray(body['data']).length, nativeBytes: Buffer.byteLength(canonical(observation, evidenceMaximumBytes)) };
    return this.store.write('task-board/native-result-page', value, mutation(runPin.objectId, 'result-page:' + hashJson(value)),
      { create: { parentId: runPin.objectId, name: 'Native result page ' + hashJson(value).slice(7) } });
  }
  private async page(runPin: TaskBoard.ObjectPin, snapshot: TaskBoard.NativeTurnSnapshot, document: Document<'task-board/native-result-page'>, collectionId: string) {
    const value = document.value;
    requireThat(document.metadata.parentId === runPin.objectId && value.runId === runPin.objectId && value.collectionId === collectionId && value.evidence.epoch === snapshot.epoch,
      'task_board_evidence_mismatch', 'Native result page must belong to its exact Run and collection.');
    const run = await this.store.read('task-board/run', runPin), params = { threadId: snapshot.threadId, turnId: snapshot.turnId, cursor: value.cursor, limit: 100, sortDirection: 'asc' };
    const observed = await this.reads.read(runPin, value.evidence.artifact, this.expected(run.value, snapshot.nativeVersion, value.evidence, 'thread/items/list', params));
    requireThat('result' in observed.reply, 'task_board_native_evidence_required', 'Native result page must have its successful original reply.');
    const result = nativeRecord(observed.reply.result), data = nativeArray(result['data']);
    requireThat(value.itemCount === data.length && value.nextCursor === pageCursor(result['nextCursor']) && value.nativeBytes === Buffer.byteLength(canonical(observed, evidenceMaximumBytes)),
      'task_board_evidence_mismatch', 'Page cursor, item count and source bytes must match their exact native observation.');
    const items = nativeTurnItems(catalogFor(snapshot.nativeVersion, { catalogHash: observed.catalogHash }).contract, observed.reply.result, snapshot.turnId);
    requireThat(Number.isFinite(Date.parse(observed.observedAt)), 'task_board_evidence_mismatch', 'A result page needs a valid native observation time.');
    return { items, observedAt: Date.parse(observed.observedAt), observationId: observed.observationId };
  }
  async transcript(runPin: TaskBoard.ObjectPin, artifact: TaskBoard.Artifact) {
    validateTaskBoard('Artifact', artifact);
    const document = await this.store.read('task-board/native-transcript', artifact.object, false), value = document.value;
    requireThat(document.metadata.parentId === runPin.objectId && document.revision.contentHash === artifact.contentHash && artifact.mediaType === 'application/json' &&
      value.phase === 'complete' && value.run.objectId === runPin.objectId && value.run.revision <= runPin.revision && value.head && value.verification,
      'task_board_evidence_mismatch', 'Final native material requires the exact complete transcript artifact.');
    return { document, ...await this.verifyTranscript(runPin, value) };
  }
  /** Validate a complete proposal before making its complete checkpoint durable. */
  async verifyTranscript(runPin: TaskBoard.ObjectPin, value: TaskBoard.NativeTranscript) {
    validateTaskBoard('NativeTranscript', value);
    requireThat(value.phase === 'complete' && value.run.objectId === runPin.objectId && value.run.revision <= runPin.revision && value.head && value.verification,
      'task_board_evidence_mismatch', 'A complete proposal needs its exact Run scope, page head and final verification.');
    const snapshot = await this.snapshot(runPin, value.snapshot), initial = snapshot.document.value;
    requireThat(terminal(initial.status) && same(initial.run, value.run) && value.verification.epoch === initial.epoch,
      'task_board_evidence_mismatch', 'A transcript must retain its original terminal snapshot and final observation epoch.');
    const pages: Document<'task-board/native-result-page'>[] = [], seen = new Set<string>(); let current: TaskBoard.ObjectPin | null = value.head;
    while (current) {
      requireThat(pages.length < value.pageCount && !seen.has(current.objectId), 'task_board_evidence_mismatch', 'Native page chain repeats or exceeds its declared bound.'); seen.add(current.objectId);
      const page: Document<'task-board/native-result-page'> = await this.store.read('task-board/native-result-page', current, false); pages.push(page); current = page.value.previous;
    }
    requireThat(pages.length === value.pageCount, 'task_board_evidence_mismatch', 'Native page chain must contain every declared page.');
    requireThat(pages.reduce((sum, page) => sum + page.value.nativeBytes, 0) === value.nativeBytes,
      'task_board_evidence_mismatch', 'Declared page bytes must fit the complete collection bound before decoding item bodies.');
    pages.reverse(); let cursor: string | null = null, bytes = 0, text = '', lastText = '', latestRead = Date.parse(initial.createdAt);
    const observations = new Set([initial.metadata.observationId, initial.turn.observationId]);
    const cursors = new Set<string>(), itemIds = new Set<string>();
    for (let index = 0; index < pages.length; index++) {
      const page = pages[index]!;
      requireThat(page.value.index === index + 1 && page.value.cursor === cursor && (index === 0 || cursor !== null) &&
        (cursor === null || !cursors.has(cursor)), 'task_board_evidence_mismatch', 'Result page indexes and cursors must form an exact advancing chain from null.');
      if (cursor !== null) cursors.add(cursor);
      const read = await this.page(value.run, initial, page, value.collectionId), items = read.items; bytes += page.value.nativeBytes;
      requireThat(read.observedAt >= latestRead && !observations.has(read.observationId), 'task_board_evidence_mismatch', 'Item pages must be successive distinct observations after the terminal snapshot.');
      latestRead = read.observedAt; observations.add(read.observationId);
      for (const item of items) {
        const id = nativeString(item['id']), identity = hashJson(id);
        requireThat(id && !itemIds.has(identity), 'task_board_evidence_mismatch', 'Native result pages must not repeat an item identity.'); itemIds.add(identity);
        if (item['type'] === 'agentMessage' && typeof item['text'] === 'string') {
          lastText = item['text'].slice(0, 262145);
          if (item['phase'] === 'final_answer' && text.length <= 262144) text += (text ? '\n\n' : '') + lastText.slice(0, 262145 - text.length);
        }
      }
      cursor = page.value.nextCursor;
    }
    requireThat(cursor === null && value.nextCursor === null && bytes === value.nativeBytes, 'task_board_evidence_mismatch', 'Final material needs native exhaustion and exact total source bytes.');
    const verification = await this.reads.read(value.run, value.verification.artifact, this.expected(snapshot.source.run.value, initial.nativeVersion, value.verification,
      'thread/turns/list', turnsParams(initial.threadId, value.verificationCursor)));
    const verifiedTurn = this.selectTurn(verification.reply, initial.turnId);
    requireThat(verifiedTurn['status'] === initial.status && Number.isFinite(Date.parse(verification.observedAt)) && Date.parse(verification.observedAt) >= latestRead && !observations.has(verification.observationId),
      'task_board_native_state_changed', 'A fresh distinct native completion observation must still match after item collection.');
    if (!text) text = lastText || 'Native turn ' + initial.status + '.';
    if (Buffer.byteLength(text) > 262144) {
      text = text.slice(0, 65536); if (/[\uD800-\uDBFF]$/.test(text)) text = text.slice(0, -1);
      text += '\n\nThe complete native output is retained in the transcript artifact.';
    }
    return { snapshot, summary: text, status: initial.status };
  }
}
