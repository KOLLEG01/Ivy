import { hashJson } from '../../../../packages/sdk/src/node.js';
import { IvyError, requireThat } from '../../../../packages/sdk/src/node.js';
import type { TaskBoard } from '../../../../packages/sdk/src/node.js';
import type { TaskBoardEngine } from './engine.js';
import { TaskBoardNativeInspector } from './native-inspector.js';
import { mutation } from './store.js';
import type { Document } from './store.js';
import { TaskBoardTurnEvidence } from './turn-evidence.js';
import { nativeRecord } from './native-intent.js';
import { TaskBoardFullTurnCollector } from './native-full-turn-collector.js';

export type NativeCollection = { kind: 'pending'; transcript: TaskBoard.ObjectPin } | { kind: 'complete'; artifact: TaskBoard.Artifact };
const artifactOf = (document: Document<'task-board/native-transcript'>): TaskBoard.Artifact => ({ object: document.pin,
  label: document.metadata.name, contentHash: document.revision.contentHash, mediaType: 'application/json' });

/** Incremental immutable page evidence and a CAS checkpoint; it never publishes workflow state. */
export class TaskBoardNativeCollector {
  readonly inspector: TaskBoardNativeInspector;
  readonly evidence: TaskBoardTurnEvidence;
  constructor(readonly engine: TaskBoardEngine) {
    this.inspector = new TaskBoardNativeInspector(engine); this.evidence = new TaskBoardTurnEvidence(engine.store, engine.settings.principalId);
  }
  async begin(runPin: TaskBoard.ObjectPin, snapshotArtifact: TaskBoard.Artifact): Promise<TaskBoard.ObjectPin> {
    await this.engine.verifyOwner();
    const snapshot = await this.evidence.snapshot(runPin, snapshotArtifact), source = snapshot.document.value;
    requireThat(source.status !== 'inProgress', 'task_board_native_order', 'Item collection requires an observed terminal turn.');
    const collectionId = mutation(runPin.objectId, 'native-collection:' + hashJson({ epoch: source.epoch, nativeVersion: source.nativeVersion, threadId: source.threadId, turnId: source.turnId }));
    const name = 'Native transcript ' + collectionId, store = this.engine.store;
    const existing = await store.named('task-board/native-transcript', name, runPin.objectId);
    if (existing) {
      requireThat(existing.value.collectionId === collectionId, 'task_board_evidence_mismatch', 'The retained transcript must preserve its collection identity.');
      return existing.pin;
    }
    const value: TaskBoard.NativeTranscript = { schemaVersion: 1, collectionId, run: source.run, snapshot: snapshotArtifact, phase: 'collecting',
      head: null, pageCount: 0, nativeBytes: 0, nextCursor: null, verification: null, verificationCursor: null, createdAt: source.createdAt, updatedAt: source.createdAt };
    try { return await store.write('task-board/native-transcript', value, mutation(collectionId, 'create'), { create: { parentId: runPin.objectId, name } }); }
    catch (error) {
      if (!(error instanceof IvyError && error.code === 'mutation_conflict')) throw error;
      const winner = await store.named('task-board/native-transcript', name, runPin.objectId);
      requireThat(winner?.value.collectionId === collectionId, 'task_board_evidence_mismatch', 'A competing collection must retain the same original identity.'); return winner.pin;
    }
  }
  private async cursors(document: Document<'task-board/native-transcript'>, snapshot: TaskBoard.NativeTurnSnapshot) {
    const value = document.value, pages: Document<'task-board/native-result-page'>[] = [], seen = new Set<string>();
    let pin = value.head;
    while (pin) {
      requireThat(pages.length < value.pageCount && !seen.has(pin.objectId), 'task_board_evidence_mismatch', 'Incomplete page chain repeats or exceeds its declared bound.'); seen.add(pin.objectId);
      const page: Document<'task-board/native-result-page'> = await this.engine.store.read('task-board/native-result-page', pin, false); pages.push(page); pin = page.value.previous;
    }
    requireThat(pages.length === value.pageCount, 'task_board_evidence_mismatch', 'Every checkpoint page must remain available.');
    pages.reverse(); const cursors = new Set<string>(); let cursor: string | null = null, bytes = 0;
    for (const [index, page] of pages.entries()) {
      requireThat(page.metadata.parentId === value.run.objectId && page.value.runId === value.run.objectId && page.value.collectionId === value.collectionId &&
        page.value.index === index + 1 && page.value.cursor === cursor && (index === 0 || cursor !== null) && page.value.evidence.epoch === snapshot.epoch && !cursors.has(hashJson(cursor)),
        'task_board_evidence_mismatch', 'Checkpoint pages must preserve their exact scope, epoch and advancing cursor chain.');
      cursors.add(hashJson(cursor)); cursor = page.value.nextCursor; bytes += page.value.nativeBytes;
    }
    requireThat(cursor === value.nextCursor && bytes === value.nativeBytes && (cursor === null || !cursors.has(hashJson(cursor))),
      'task_board_evidence_mismatch', 'Checkpoint cursor and bytes must match its retained page chain.'); return cursors;
  }
  async advance(runPin: TaskBoard.ObjectPin, collectionObjectId: string, budget = 8): Promise<NativeCollection> {
    requireThat(Number.isSafeInteger(budget) && budget >= 1 && budget <= 64, 'task_board_inspection_budget', 'One collection pass allows one to 64 item pages.');
    await this.engine.verifyOwner(); const store = this.engine.store;
    let document = await store.read('task-board/native-transcript', collectionObjectId, false);
    requireThat(document.metadata.parentId === runPin.objectId && document.value.run.objectId === runPin.objectId && document.value.run.revision <= runPin.revision,
      'task_board_evidence_mismatch', 'Collection must retain its original Run scope.');
    if (document.value.phase === 'complete') {
      const artifact = artifactOf(document); await this.evidence.transcript(runPin, artifact); return { kind: 'complete', artifact };
    }
    const snapshot = (await this.evidence.snapshot(runPin, document.value.snapshot)).document.value;
    requireThat(snapshot.status !== 'inProgress' && document.value.verification === null && document.value.verificationCursor === null &&
      hashJson(snapshot.run) === hashJson(document.value.run), 'task_board_evidence_mismatch', 'An incomplete collection must retain its original terminal snapshot.');
    const full = new TaskBoardFullTurnCollector(this.engine, this.inspector);
    if (document.value.pageCount === 0) {
      const retained = await full.retained(runPin, document.value);
      if (retained) return { kind: 'complete', artifact: retained };
    }
    const context = await this.inspector.context(document.value.run);
    requireThat(context.epoch === snapshot.epoch, 'task_board_native_epoch_changed', 'A replacement owner cannot append to an earlier native collection.');
    const cursors = await this.cursors(document, snapshot);
    const write = async (value: TaskBoard.NativeTranscript) => {
      await this.engine.verifyOwner();
      const pin = await store.write('task-board/native-transcript', value, mutation(value.collectionId, 'checkpoint:' + hashJson({ pin: document.pin, value })),
        { objectId: document.pin.objectId, expectedRevision: document.pin.revision });
      document = await store.read('task-board/native-transcript', pin, false);
    };
    for (let index = 0; index < budget && (document.value.pageCount === 0 || document.value.nextCursor !== null); index++) {
      const current = document.value;
      requireThat(current.pageCount < 1024, 'task_board_native_collection_limit', 'Native collection exceeded its 1024-page bound.');
      requireThat(!cursors.has(hashJson(current.nextCursor)), 'task_board_native_cursor_loop', 'A native item cursor must advance.');
      const observed = await this.inspector.read(context, 'thread/items/list', { threadId: snapshot.threadId, turnId: snapshot.turnId, cursor: current.nextCursor, limit: 100, sortDirection: 'asc' }, true);
      if ('error' in observed.reply && observed.reply.error.code === -32601 && current.pageCount === 0) {
        const artifact = await full.collect(runPin, document, observed, budget);
        return artifact ? { kind: 'complete', artifact } : { kind: 'pending', transcript: document.pin };
      }
      requireThat('result' in observed.reply, 'task_board_native_read_failed', 'Native inspection returned its original error; no successful page is available.');
      const head = await this.evidence.savePage(current.run, snapshot, current.collectionId, current.head, current.pageCount + 1, current.nextCursor, observed);
      const page = await store.read('task-board/native-result-page', head, false); cursors.add(hashJson(current.nextCursor));
      requireThat(page.value.nextCursor === null || !cursors.has(hashJson(page.value.nextCursor)), 'task_board_native_cursor_loop', 'The native item reply repeated a collected cursor.');
      requireThat(current.nativeBytes + page.value.nativeBytes <= 1073741824, 'task_board_native_collection_limit', 'Native collection exceeded its one-GiB page observation bound.');
      await write({ ...current, head, pageCount: current.pageCount + 1, nativeBytes: current.nativeBytes + page.value.nativeBytes,
        nextCursor: page.value.nextCursor, updatedAt: observed.observedAt });
    }
    if (document.value.nextCursor !== null) return { kind: 'pending', transcript: document.pin };
    // Locate again: newer turns may have shifted the original turn to another page.
    const found = await this.inspector.find(document.value.run, budget);
    if (found.kind === 'pending') return { kind: 'pending', transcript: document.pin };
    requireThat(found.kind === 'found', 'task_board_turn_unavailable', 'The original terminal turn is unavailable for final verification.');
    requireThat(found.turns.epoch === snapshot.epoch, 'task_board_native_epoch_changed', 'Final verification must remain in the collection epoch.');
    const observed = found.turns, verificationArtifact = await this.evidence.reads.save(document.value.run, observed, { ...context,
      callerPrincipalId: this.engine.settings.principalId, observationId: observed.observationId, method: observed.method, params: observed.params });
    const value: TaskBoard.NativeTranscript = { ...document.value, phase: 'complete', verification: { artifact: verificationArtifact, observationId: observed.observationId, epoch: observed.epoch },
      verificationCursor: nativeRecord(observed.params)['cursor'] as string | null, updatedAt: observed.observedAt };
    await this.evidence.verifyTranscript(runPin, value); // Every page and fresh terminal status before the complete write.
    await write(value); return { kind: 'complete', artifact: artifactOf(document) };
  }
}
