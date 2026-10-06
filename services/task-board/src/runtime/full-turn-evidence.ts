import { canonical } from '../../../../packages/sdk/src/node.js';
import { requireThat } from '../../../../packages/sdk/src/node.js';
import { validateTaskBoard } from '../../../../packages/sdk/src/node.js';
import type { Agent, TaskBoard, Wire } from '../../../../packages/sdk/src/node.js';
import { same } from './checks.js';
import { evidenceMaximumBytes } from './evidence-bytes.js';
import { nativeArray, nativeRecord, nativeString } from './native-intent.js';
import type { TaskBoardStore } from './store.js';
import { TaskBoardTurnEvidence } from './turn-evidence.js';

export const fullTurnParams = (threadId: string, cursor: string | null, full: boolean) => ({ threadId, cursor, limit: 1, itemsView: full ? 'full' : 'notLoaded', sortDirection: 'desc' });
export const firstItemsParams = (threadId: string, turnId: string) => ({ threadId, turnId, cursor: null, limit: 100, sortDirection: 'asc' });

/** A full-view native response is a separate proof type, never a synthetic item page. */
export class TaskBoardFullTurnEvidence {
  readonly turns: TaskBoardTurnEvidence;
  constructor(readonly store: TaskBoardStore, readonly principalId: string) { this.turns = new TaskBoardTurnEvidence(store, principalId); }
  async verify(runPin: TaskBoard.ObjectPin, value: TaskBoard.NativeFullTurnTranscript) {
    validateTaskBoard('NativeFullTurnTranscript', value);
    const snapshot = await this.turns.snapshot(runPin, value.snapshot), initial = snapshot.document.value;
    requireThat(same(value.run, initial.run) && ['completed', 'failed', 'interrupted'].includes(initial.status), 'task_board_evidence_mismatch', 'Full evidence requires the original terminal snapshot and Run scope.');
    const read = async (ref: TaskBoard.ReadEvidenceRef, method: Agent.ReadObservation['method'], params: Wire.Json) => {
      requireThat(ref.epoch === initial.epoch, 'task_board_native_epoch_changed', 'Full evidence must retain one native epoch.');
      return this.turns.reads.read(value.run, ref.artifact, { observationId: ref.observationId, callerPrincipalId: this.principalId,
        serviceNodeId: initial.serviceNodeId, nativeVersion: initial.nativeVersion, epoch: initial.epoch, method, params });
    };
    const unsupported = await read(value.unsupportedItems, 'thread/items/list', firstItemsParams(initial.threadId, initial.turnId));
    requireThat('error' in unsupported.reply && unsupported.reply.error.code === -32601, 'task_board_native_evidence_required', 'Fallback requires the exact native unsupported-method error, not an arbitrary failed read.');
    const full = await read(value.fullTurn, 'thread/turns/list', fullTurnParams(initial.threadId, value.fullTurnCursor, true));
    const verification = await read(value.verification, 'thread/turns/list', fullTurnParams(initial.threadId, value.verificationCursor, false));
    const select = (observation: Agent.ReadObservation) => {
      requireThat('result' in observation.reply, 'task_board_native_evidence_required', 'A full transcript requires successful original native turn reads.');
      const data = nativeArray(nativeRecord(observation.reply.result)['data']);
      requireThat(data.length === 1, 'task_board_turn_mismatch', 'The exact full-turn page must contain one turn.');
      const turn = nativeRecord(data[0]);
      requireThat(turn['id'] === initial.turnId && turn['status'] === initial.status, 'task_board_native_state_changed', 'The original terminal turn must remain unchanged.');
      return turn;
    };
    const turn = select(full); select(verification);
    requireThat(turn['itemsView'] === 'full', 'task_board_native_evidence_required', 'Only an explicitly complete native item view can publish a full transcript.');
    requireThat(Buffer.byteLength(canonical(full, evidenceMaximumBytes)) === value.fullTurnBytes, 'task_board_evidence_mismatch', 'Full native observation bytes must match their retained length.');
    let previous = Date.parse(initial.createdAt); const identities = new Set([initial.metadata.observationId, initial.turn.observationId]);
    for (const observation of [unsupported, full, verification]) {
      const at = Date.parse(observation.observedAt);
      requireThat(Number.isFinite(at) && at >= previous && !identities.has(observation.observationId), 'task_board_evidence_mismatch', 'Full transcript observations must be successive, distinct and follow the terminal snapshot.');
      previous = at; identities.add(observation.observationId);
    }
    requireThat(value.createdAt === verification.observedAt, 'task_board_evidence_mismatch', 'Completion time belongs to the final native verification.');
    const ids = new Set<string>(), texts: string[] = []; let lastText = "";
    for (const raw of nativeArray(turn['items'])) {
      const item = nativeRecord(raw), id = nativeString(item['id']);
      requireThat(id && !ids.has(id), 'task_board_evidence_mismatch', 'Complete native items must retain unique nonempty identities.'); ids.add(id);
      if (item['type'] === 'agentMessage') { lastText = nativeString(item['text']); if (item['phase'] === 'final_answer') texts.push(lastText); }
    }
    let summary = texts.join('\n\n') || lastText || 'Native turn ' + initial.status + '.';
    if (Buffer.byteLength(summary) > 262144) {
      summary = summary.slice(0, 65536); if (/[\uD800-\uDBFF]$/.test(summary)) summary = summary.slice(0, -1);
      summary += '\n\nThe complete native output is retained in the transcript artifact.';
    }
    return { snapshot, summary, status: initial.status };
  }
  async transcript(runPin: TaskBoard.ObjectPin, artifact: TaskBoard.Artifact) {
    validateTaskBoard('Artifact', artifact);
    const document = await this.store.read('task-board/native-full-turn-transcript', artifact.object, false);
    requireThat(document.metadata.parentId === runPin.objectId && document.revision.contentHash === artifact.contentHash && artifact.mediaType === 'application/json', 'task_board_evidence_mismatch', 'The full transcript must retain its exact immutable artifact and parent.');
    return { document, ...await this.verify(runPin, document.value) };
  }
}

export async function verifyNativeTranscript(store: TaskBoardStore, principalId: string, run: TaskBoard.ObjectPin, artifact: TaskBoard.Artifact) {
  const key = store.localKey(artifact.object.objectId);
  if (key === 'task-board/native-full-turn-transcript') return new TaskBoardFullTurnEvidence(store, principalId).transcript(run, artifact);
  return new TaskBoardTurnEvidence(store, principalId).transcript(run, artifact);
}
