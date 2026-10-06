import { hashJson } from '../../../../packages/sdk/src/node.js';
import { requireThat } from '../../../../packages/sdk/src/node.js';
import { validateAgent } from '../../../../packages/sdk/src/node.js';
import type { Agent, TaskBoard, Wire } from '../../../../packages/sdk/src/node.js';
import { serviceTools } from '../../../../packages/sdk/src/client.js';
import { validateNativeStatus } from '../../../../packages/sdk/src/native-evidence.js';
import { same } from './checks.js';
import { catalogFor } from './evidence.js';
import type { TaskBoardEngine } from './engine.js';
import { nativeArray, nativeRecord } from './native-intent.js';
import { TaskBoardReadEvidence } from './read-evidence.js';
import { mutation } from './store.js';
import { pageCursor, turnsParams } from './turn-evidence.js';

export interface NativeInspectionContext {
  run: TaskBoard.ObjectPin; nativeVersion: string; catalogSourceHash: string; serviceNodeId: string; epoch: string; threadId: string; turnId: string;
}
export type NativeInspection = { kind: 'found'; metadata: Agent.ReadObservation; turns: Agent.ReadObservation }
  | { kind: 'pending'; search: TaskBoard.ObjectPin } | { kind: 'absent' };

/** Read-only native inspection. Search checkpoints confer no execution or completion authority. */
export class TaskBoardNativeInspector {
  readonly reads: TaskBoardReadEvidence;
  constructor(readonly engine: TaskBoardEngine) { this.reads = new TaskBoardReadEvidence(engine.store); }
  private async management(node: string, name: string, args: Wire.Json) {
    return serviceTools(this.engine.store.client, node, [{ namespace: 'agent', interfaceVersion: '1.0.0' }]).read('agent.' + name, args);
  }
  async context(runPin: TaskBoard.ObjectPin): Promise<NativeInspectionContext> {
    await this.engine.verifyOwner();
    const run = await this.engine.store.read('task-board/run', runPin), plan = run.value.plan;
    const primary = run.value.primaryResourceRef;
    requireThat(primary?.namespace === 'codex' && primary.kind === 'thread' && primary.serviceNodeId === run.value.target.serviceNodeId && run.value.turnId && run.value.nativeEpoch,
      'task_board_turn_mismatch', 'Inspection requires an already saved original native primary and turn.');
    const raw = await this.management(run.value.target.serviceNodeId, 'status', {});
    const catalog = catalogFor(plan.nativeVersion, { sourceHash: plan.catalogSourceHash }).catalog;
    validateNativeStatus(raw, { serviceNodeId: run.value.target.serviceNodeId, hostId: run.value.target.hostId, nativeVersion: plan.nativeVersion,
      nativeExecutableHash: catalog.nativeExecutableHash, catalogHash: hashJson(catalog) }, 'task_board_native_unavailable'); const status = raw;
    return { run: runPin, nativeVersion: plan.nativeVersion, catalogSourceHash: plan.catalogSourceHash, serviceNodeId: status.serviceNodeId, epoch: status.epoch, threadId: primary.nativeId, turnId: run.value.turnId };
  }
  async ownerStopped(run: TaskBoard.Run): Promise<boolean> {
    if (!run.nativeEpoch) return false;
    const raw = await this.management(run.target.serviceNodeId, 'status', {});
    const catalog = catalogFor(run.plan.nativeVersion, { sourceHash: run.plan.catalogSourceHash }).catalog;
    validateNativeStatus(raw, { serviceNodeId: run.target.serviceNodeId, hostId: run.target.hostId,
      nativeVersion: run.plan.nativeVersion, nativeExecutableHash: catalog.nativeExecutableHash,
      catalogHash: hashJson(catalog) }, 'task_board_native_unavailable');
    const status = raw as Agent.Status;
    return status.state === 'ready' && !!status.epoch && status.epoch !== run.nativeEpoch &&
      status.connection?.mode === 'owned-stdio' && status.connection.ownsServer &&
      !!status.processStops?.some(value => value.epoch === run.nativeEpoch);
  }
  async read(context: NativeInspectionContext, method: Agent.ReadObservation['method'], params: Wire.Json, allowError = false): Promise<Agent.ReadObservation> {
    await this.engine.verifyOwner();
    const raw = await this.management(context.serviceNodeId, 'read', { nativeVersion: context.nativeVersion, method, params });
    validateAgent('ReadObservation', raw); const observed = raw as Agent.ReadObservation;
    this.reads.verify(observed, { ...context, observationId: observed.observationId, callerPrincipalId: this.engine.settings.principalId, method, params }, context.catalogSourceHash);
    requireThat(allowError || 'result' in observed.reply, 'task_board_native_read_failed', 'Native inspection returned its original error; no successful page is available.');
    requireThat(Number.isFinite(Date.parse(observed.observedAt)), 'task_board_evidence_mismatch', 'Native inspection requires a valid observation time.');
    return observed;
  }
  private matches(observation: Agent.ReadObservation, turnId: string) {
    requireThat('result' in observation.reply, 'task_board_native_read_failed', 'Native turn inspection requires a successful reply.');
    const page = nativeRecord(observation.reply.result), data = nativeArray(page['data']);
    requireThat(data.length <= 100, 'task_board_native_page_limit', 'The native turn page exceeds the requested item bound.');
    const matches = data.map(nativeRecord).filter(turn => turn['id'] === turnId);
    requireThat(matches.length <= 1, 'task_board_turn_mismatch', 'The known native turn may appear only once in its page.');
    return { found: matches.length === 1, nextCursor: pageCursor(page['nextCursor']) };
  }
  async find(runPin: TaskBoard.ObjectPin, budget = 8, pageSize: 1 | 100 = 100): Promise<NativeInspection> {
    requireThat(Number.isSafeInteger(budget) && budget >= 1 && budget <= 64, 'task_board_inspection_budget', 'One inspection pass allows one to 64 search pages.');
    const context = await this.context(runPin), store = this.engine.store;
    const name = 'Native turn search ' + hashJson({ ...context, run: runPin.objectId, ...(pageSize === 1 ? { pageSize } : {}) }).slice(7);
    const saved = await store.named('task-board/native-turn-search', name, runPin.objectId);
    if (saved) {
      const old = saved.value, original = await store.read('task-board/run', old.run), current = await store.read('task-board/run', runPin);
      requireThat(old.run.objectId === runPin.objectId && old.run.revision <= runPin.revision && same(original.value.originalRequest, current.value.originalRequest) &&
        old.threadId === context.threadId && old.turnId === context.turnId && old.nativeVersion === context.nativeVersion && old.serviceNodeId === context.serviceNodeId && old.epoch === context.epoch &&
        old.pageCount === old.visitedCursorHashes.length && original.value.primaryResourceRef?.nativeId === context.threadId && original.value.turnId === context.turnId,
        'task_board_evidence_mismatch', 'Retained search must preserve the exact known execution and bounded cursor history.');
    }
    const continuing = saved?.value.phase === 'searching', visited = new Set(continuing ? saved.value.visitedCursorHashes : []);
    let cursor = continuing ? saved.value.nextCursor : null;
    const createdAt = continuing ? saved.value.createdAt : new Date().toISOString();
    const checkpoint = async (finished: boolean): Promise<TaskBoard.ObjectPin> => {
      await this.engine.verifyOwner();
      const { catalogSourceHash: _catalogSourceHash, ...savedContext } = context;
      const value: TaskBoard.NativeTurnSearch = { schemaVersion: 1, ...savedContext, run: saved?.value.run ?? runPin,
        phase: finished ? 'finished' : 'searching', nextCursor: finished ? null : cursor, pageCount: visited.size,
        visitedCursorHashes: [...visited], createdAt, updatedAt: new Date().toISOString() };
      return store.write('task-board/native-turn-search', value, saved ? mutation(name, 'advance:' + hashJson({ pin: saved.pin, value })) : mutation(name, 'create'),
        saved ? { objectId: saved.pin.objectId, expectedRevision: saved.pin.revision } : { create: { parentId: runPin.objectId, name } });
    };
    for (let index = 0; index < budget; index++) {
      requireThat(visited.size < 1024, 'task_board_native_search_limit', 'The known native turn search exceeded 1024 pages.');
      const identity = hashJson(cursor);
      requireThat(!visited.has(identity), 'task_board_native_cursor_loop', 'The native turn search repeated an opaque cursor.');
      const params = { ...turnsParams(context.threadId, cursor), limit: pageSize };
      const turns = await this.read(context, 'thread/turns/list', params);
      requireThat('result' in turns.reply && nativeArray(nativeRecord(turns.reply.result)['data']).length <= pageSize, 'task_board_native_page_limit', 'The native turn page exceeds its exact requested bound.');
      const page = this.matches(turns, context.turnId); visited.add(identity);
      if (page.found) {
        const metadata = await this.read(context, 'thread/read', { threadId: context.threadId, includeTurns: false });
        const fresh = await this.read(context, 'thread/turns/list', params);
        requireThat('result' in fresh.reply && nativeArray(nativeRecord(fresh.reply.result)['data']).length <= pageSize, 'task_board_native_page_limit', 'The fresh native turn page exceeds its exact requested bound.');
        requireThat(this.matches(fresh, context.turnId).found, 'task_board_native_state_changed', 'The known turn moved during inspection; inspect again before publication.');
        requireThat('result' in metadata.reply, 'task_board_native_read_failed', 'Native metadata requires its successful reply.');
        const thread = nativeRecord(nativeRecord(metadata.reply.result)['thread']);
        requireThat(thread['id'] === context.threadId && thread['ephemeral'] === false && Date.parse(metadata.observedAt) <= Date.parse(fresh.observedAt),
          'task_board_evidence_mismatch', 'Fresh metadata must retain the exact durable primary and precede its selected turn observation.');
        if (saved) await checkpoint(true);
        return { kind: 'found', metadata, turns: fresh };
      }
      cursor = page.nextCursor;
      if (cursor === null) { if (saved) await checkpoint(true); return { kind: 'absent' }; }
      requireThat(!visited.has(hashJson(cursor)), 'task_board_native_cursor_loop', 'The native turn search repeated an opaque cursor.');
    }
    return { kind: 'pending', search: await checkpoint(false) };
  }
}
