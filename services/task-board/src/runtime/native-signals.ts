import { hashJson } from '../../../../packages/sdk/src/node.js';
import { IvyError, requireThat } from '../../../../packages/sdk/src/node.js';
import { validateAgent } from '../../../../packages/sdk/src/node.js';
import type { Agent, TaskBoard, Wire } from '../../../../packages/sdk/src/node.js';
import { serviceTools } from '../../../../packages/sdk/src/client.js';
import type { TaskBoardEngine } from './engine.js';
import { nativeRecord } from './native-intent.js';
import { TaskBoardNativeInspector } from './native-inspector.js';
import { finishedRun } from './native-publication.js';
import { mutation } from './store.js';

/** Advisory attention only. Native payloads, replies and answer authority stay with AgentManager. */
export class TaskBoardNativeSignals {
  readonly inspector: TaskBoardNativeInspector;
  private readonly cursors = new Map<string, { epoch: string; sequence: number }>();
  constructor(readonly engine: TaskBoardEngine) { this.inspector = new TaskBoardNativeInspector(engine); }
  private async read(node: string, name: 'inputs' | 'notifications', args: Wire.Json) {
    return serviceTools(this.engine.store.client, node, [{ namespace: 'agent', interfaceVersion: '1.0.0' }]).read('agent.' + name, args);
  }
  async step(runId: string): Promise<TaskBoard.ObjectPin | null> {
    const store = this.engine.store, run = await store.read('task-board/run', runId);
    if (finishedRun(run.value) || !run.value.turnId) { this.cursors.delete(runId); return null; }
    const context = await this.inspector.context(run.pin), name = 'Native signals';
    const saved = await store.named('task-board/native-signals', name, runId);
    requireThat(!saved || saved.metadata.parentId === runId && saved.value.run.objectId === runId && saved.value.run.revision <= run.pin.revision &&
      saved.value.serviceNodeId === context.serviceNodeId && saved.value.nativeVersion === context.nativeVersion && saved.value.threadId === context.threadId && saved.value.turnId === context.turnId,
      'task_board_identity_conflict', 'A native signals checkpoint must retain its original Run, owner and native context.');
    const epochChanged = !!saved && saved.value.epoch !== context.epoch;
    const volatile = this.cursors.get(runId);
    const afterSequence = Math.max(!saved || epochChanged ? 0 : saved.value.notificationCursor,
      volatile?.epoch === context.epoch ? volatile.sequence : 0);
    const rawInputs = await this.read(context.serviceNodeId, 'inputs', { limit: 128 }); validateAgent('PendingInputPage', rawInputs);
    const rawPage = await this.read(context.serviceNodeId, 'notifications', { afterSequence, limit: 100 }); validateAgent('NotificationPage', rawPage);
    const pending = rawInputs as Agent.PendingInputPage, page = rawPage as Agent.NotificationPage;
    requireThat(pending.epoch === context.epoch && page.epoch === context.epoch, 'task_board_native_epoch_changed', 'Native signals changed connection epoch while being read.');
    const inputs: TaskBoard.NativeSignals['inputs'] = [];
    for (const input of pending.items) {
      requireThat(input.identity.serviceNodeId === context.serviceNodeId, 'task_board_target_mismatch', 'A pending native request must identify its queried owner.');
      if (context.epoch !== run.value.nativeEpoch || input.identity.epoch !== context.epoch || input.threadId !== context.threadId ||
        input.turnId !== null && input.turnId !== context.turnId || !['pending', 'answering'].includes(input.state)) continue;
      inputs.push({ identity: input.identity, method: input.method, turnId: input.turnId, state: input.state as 'pending' | 'answering', observedAt: input.observedAt });
    }
    inputs.sort((a, b) => hashJson(a.identity).localeCompare(hashJson(b.identity)));
    requireThat(new Set(inputs.map(input => hashJson(input.identity))).size === inputs.length, 'task_board_native_input_conflict', 'Current native input identities must be unique.');
    const activity = new Map((saved?.value.activity ?? []).map(item => [hashJson({ epoch: item.epoch, sequence: item.sequence }), item]));
    let sequence = afterSequence;
    for (const item of page.items) {
      requireThat(item.serviceNodeId === context.serviceNodeId && item.nativeVersion === context.nativeVersion && item.sequence > sequence,
        'task_board_native_notification_conflict', 'Native notification pages must retain the selected owner/version and increasing original sequence.');
      sequence = item.sequence;
      const params = nativeRecord(item.params), turnId = typeof params['turnId'] === 'string' ? params['turnId'] : nativeRecord(params['turn'])['id'];
      if (item.epoch !== context.epoch || params['threadId'] !== context.threadId || turnId !== undefined && turnId !== null && turnId !== context.turnId) continue;
      activity.set(hashJson({ epoch: item.epoch, sequence: item.sequence }), { epoch: item.epoch, sequence: item.sequence, method: item.method, observedAt: item.observedAt });
    }
    requireThat(page.throughSequence >= sequence || page.gap && page.items.length === 0 && !page.hasMore,
      'task_board_cursor_regression', 'Only an explicit empty journal gap can lower an observed notification cursor.');
    requireThat(!page.hasMore || page.throughSequence > afterSequence, 'task_board_cursor_regression', 'A pending notification page must advance its cursor.');
    // Fence a process replacement during the two queries. Retained observations still never grant
    // answerability; the UI obtains a fresh exact-identity request before sending an answer.
    requireThat((await this.inspector.context(run.pin)).epoch === context.epoch, 'task_board_native_epoch_changed', 'The selected owner changed before saving native signals.');
    const advance = () => {
      if (this.cursors.size >= 1024 && !this.cursors.has(runId)) this.cursors.delete(this.cursors.keys().next().value!);
      this.cursors.set(runId, { epoch: context.epoch, sequence: page.throughSequence });
    };
    const material = { schemaVersion: 1 as const, run: run.pin, serviceNodeId: context.serviceNodeId, nativeVersion: context.nativeVersion,
      epoch: context.epoch, threadId: context.threadId, turnId: context.turnId, inputsTruncated: pending.truncated, inputs,
      notificationCursor: page.throughSequence, notificationGap: !!saved?.value.notificationGap || epochChanged || page.gap,
      notificationsPending: page.hasMore, activity: [...activity.values()].slice(-100) };
    if (saved) {
      const { observedAt: _observedAt, ...previous } = saved.value;
      const comparable = { ...material, run: saved.value.run, notificationCursor: saved.value.notificationCursor,
        notificationsPending: saved.value.notificationsPending };
      if (hashJson(previous) === hashJson(comparable)) { advance(); return saved.pin; }
    }
    await this.engine.verifyOwner();
    const value: TaskBoard.NativeSignals = { ...material, observedAt: new Date().toISOString() };
    try {
      const pin = await store.write('task-board/native-signals', value, mutation(runId, 'signals:' + hashJson({ previous: saved?.pin ?? null, material })),
        saved ? { objectId: saved.pin.objectId, expectedRevision: saved.pin.revision } : { create: { parentId: runId, name } });
      advance();
      return pin;
    } catch (error) {
      if (!(error instanceof IvyError && ['revision_conflict', 'mutation_conflict'].includes(error.code))) throw error;
      this.cursors.delete(runId);
      const winner = await store.named('task-board/native-signals', name, runId); if (!winner) throw error;
      return winner.pin; // A later pass validates the winning checkpoint and observes fresh owner state.
    }
  }
}
