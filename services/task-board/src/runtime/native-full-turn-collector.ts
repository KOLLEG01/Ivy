import { canonical } from '../../../../packages/sdk/src/node.js';
import { IvyError, requireThat } from '../../../../packages/sdk/src/node.js';
import type { Agent, TaskBoard } from '../../../../packages/sdk/src/node.js';
import { evidenceMaximumBytes } from './evidence-bytes.js';
import { firstItemsParams, fullTurnParams, TaskBoardFullTurnEvidence } from './full-turn-evidence.js';
import type { TaskBoardEngine } from './engine.js';
import type { TaskBoardNativeInspector } from './native-inspector.js';
import { nativeRecord } from './native-intent.js';
import { mutation } from './store.js';
import type { Document } from './store.js';

/** A bounded alternative only after the first native item read is explicitly unsupported. */
export class TaskBoardFullTurnCollector {
  readonly evidence: TaskBoardFullTurnEvidence;
  constructor(readonly engine: TaskBoardEngine, readonly inspector: TaskBoardNativeInspector) {
    this.evidence = new TaskBoardFullTurnEvidence(engine.store, engine.settings.principalId);
  }
  private name(collectionId: string) { return 'Native full turn transcript ' + collectionId; }
  async retained(run: TaskBoard.ObjectPin, collection: TaskBoard.NativeTranscript): Promise<TaskBoard.Artifact | null> {
    const document = await this.engine.store.named('task-board/native-full-turn-transcript', this.name(collection.collectionId), run.objectId);
    if (!document) return null;
    requireThat(document.value.collectionId === collection.collectionId && document.value.snapshot.contentHash === collection.snapshot.contentHash,
      'task_board_evidence_mismatch', 'A retained full transcript must belong to the original collection and snapshot.');
    const artifact = { object: document.pin, label: document.metadata.name, contentHash: document.revision.contentHash, mediaType: 'application/json' };
    await this.evidence.transcript(run, artifact); return artifact;
  }
  async collect(run: TaskBoard.ObjectPin, collection: Document<'task-board/native-transcript'>, unsupported: Agent.ReadObservation, budget: number): Promise<TaskBoard.Artifact | null> {
    const value = collection.value, snapshot = (await this.evidence.turns.snapshot(run, value.snapshot)).document.value;
    requireThat(value.phase === 'collecting' && value.pageCount === 0 && value.head === null && value.nextCursor === null &&
      'error' in unsupported.reply && unsupported.reply.error.code === -32601, 'task_board_native_evidence_required', 'Only an unsupported first item page permits full-turn collection.');
    const context = await this.inspector.context(value.run);
    requireThat(context.epoch === snapshot.epoch, 'task_board_native_epoch_changed', 'Full-turn collection requires the original native epoch.');
    const save = async (observed: Agent.ReadObservation, params: Agent.ReadObservation['params']): Promise<TaskBoard.ReadEvidenceRef> => ({
      artifact: await this.evidence.turns.reads.save(value.run, observed, { ...context, callerPrincipalId: this.engine.settings.principalId,
        observationId: observed.observationId, method: observed.method, params }), observationId: observed.observationId, epoch: observed.epoch });
    // Preserve the original error even when a bounded search must continue in another pass.
    const unsupportedItems = await save(unsupported, firstItemsParams(snapshot.threadId, snapshot.turnId));
    const found = await this.inspector.find(value.run, budget, 1);
    if (found.kind === 'pending') return null;
    requireThat(found.kind === 'found', 'task_board_turn_unavailable', 'The original terminal turn is unavailable for full evidence.');
    const cursor = nativeRecord(found.turns.params)['cursor'] as string | null;
    const fullParams = fullTurnParams(snapshot.threadId, cursor, true), verifyParams = fullTurnParams(snapshot.threadId, cursor, false);
    const full = await this.inspector.read(context, 'thread/turns/list', fullParams);
    const fullTurn = await save(full, fullParams);
    // Independent read after the full response. A shifted page is rejected and located again on retry.
    const checked = await this.inspector.read(context, 'thread/turns/list', verifyParams);
    const verification = await save(checked, verifyParams);
    const complete: TaskBoard.NativeFullTurnTranscript = { schemaVersion: 1, collectionId: value.collectionId, run: value.run, snapshot: value.snapshot,
      unsupportedItems, fullTurn, fullTurnCursor: cursor, fullTurnBytes: Buffer.byteLength(canonical(full, evidenceMaximumBytes)),
      verification, verificationCursor: cursor, createdAt: checked.observedAt };
    await this.evidence.verify(run, complete); await this.engine.verifyOwner();
    try {
      await this.engine.store.write('task-board/native-full-turn-transcript', complete, mutation(value.collectionId, 'full-turn:create'),
        { create: { parentId: run.objectId, name: this.name(value.collectionId) } });
    } catch (error) {
      if (!(error instanceof IvyError && error.code === 'mutation_conflict')) throw error;
      // A competing writer won this stable identity; verify its complete original evidence below.
    }
    const artifact = await this.retained(run, value);
    requireThat(artifact, 'task_board_evidence_mismatch', 'A completed full transcript must remain available.'); return artifact;
  }
}
