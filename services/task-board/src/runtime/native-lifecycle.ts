import { hashJson, IvyError, requireThat, validateAgent } from '../../../../packages/sdk/src/node.js';
import type { Agent, TaskBoard, Wire } from '../../../../packages/sdk/src/node.js';
import { scopedOperationId, serviceTools } from '../../../../packages/sdk/src/client.js';
import { NativeOwner } from '../../../../packages/sdk/src/native-owner.js';
import { reconcileNativeOperation } from '../../../../packages/sdk/src/native-operation.js';
import type { TaskBoardEngine, Publication } from './engine.js';
import { nativeRecord, nativeString } from './native-intent.js';
import { missingNativeThread, unstartedMissingContext } from './native-recovery.js';
import { same } from './checks.js';
import { readPlan } from './native-plan.js';
import { TaskBoardScheduler } from './scheduler.js';
import type { Document, TaskBoardStore } from './store.js';

type Operation = Document<'task-board/operation'>;
type Primary = Wire.ResourceRef;
const callKey = (method: string, primary: Primary) => method + ':' + hashJson(primary).slice(7);
const missing = (reply: Agent.Reply | null) => !!reply && 'error' in reply &&
  /not found|does not exist|no rollout found|no such file/i.test(reply.error.message);

/** Ticket lifecycle actions retain native requests and receipts in the same Operation.
 * A pending action holds only this ticket's gate; unrelated tickets continue normally. */
export class TaskBoardNativeLifecycle {
  constructor(readonly engine: TaskBoardEngine) {}

  private async retained(operation: Operation, key: string) {
    return (await this.engine.store.read('task-board/operation', operation.pin.objectId)).value.nativeCalls?.find(call => call.key === key);
  }

  private async owner(serviceNodeId: string): Promise<NativeOwner> {
    const store = this.engine.store;
    const node = await store.client.request('serviceNodes.get', { serviceNodeId });
    const status = await serviceTools(store.client, serviceNodeId, [{ namespace: 'agent', interfaceVersion: '1.0.0' }]).read('agent.status', {}) as Agent.Status;
    validateAgent('Status', status);
    requireThat(node.serviceName === 'agent-manager' && status.serviceNodeId === serviceNodeId &&
      status.hostId === node.hostId && status.nativeVersion === node.nativeVersion && status.state === 'ready' && status.epoch,
      'task_board_native_unavailable', 'The native owner must be ready before changing its ticket context.');
    return new NativeOwner(store.client, this.engine.settings.principalId, {
      serviceNodeId, hostId: status.hostId, nativeVersion: status.nativeVersion,
      nativeExecutableHash: status.nativeExecutableHash, catalogHash: status.catalogHash,
    });
  }

  private async call(operation: Operation, key: string, prepare: () => Promise<{
    owner: NativeOwner; method: TaskBoard.NativeLifecycleCall['method']; params: Wire.Json;
  }>): Promise<Agent.Operation> {
    const store = this.engine.store;
    let saved = await this.retained(operation, key);
    if (!saved) {
      const draft = await prepare(), binding = await draft.owner.binding(draft.method);
      requireThat(binding.definition.interfaceVersion === draft.owner.target.nativeVersion,
        'task_board_native_intent_mismatch', 'The lifecycle method must belong to the selected native version.');
      saved = await this.engine.operations.prepareNative(operation.pin.objectId, {
        key, owner: draft.owner.target as TaskBoard.NativeLifecycleCall['owner'],
        operationId: await scopedOperationId(store.client, ['task-board-lifecycle', operation.pin.objectId, key]),
        method: draft.method, params: draft.params, definitionHash: binding.definitionHash, observation: null,
      });
    }
    const owner = new NativeOwner(store.client, this.engine.settings.principalId, saved.owner);
    const result = await reconcileNativeOperation({ owner, call: saved,
      journal: { current: saved, previous: saved.observation, wasObserved: saved.observation !== null,
        retain: observed => this.engine.operations.observeNative(operation.pin.objectId, key, observed) },
      onAbsent: async (original, absence) => {
        const status = await owner.status();
        requireThat(status.epoch === absence.epoch, 'task_board_native_epoch_changed', 'Native absence and readiness must identify the same owner epoch.');
        return owner.dispatch(original, async () => {
          await this.engine.verifyOwner();
          const request = operation.value.request;
          requireThat('taskId' in request, 'task_board_native_intent_mismatch', 'Native lifecycle work requires its original ticket.');
          const current = await store.read('task-board/task', request.taskId, true, true);
          requireThat(current.pin.revision === request.expectedRevision && !current.value.claim && !current.value.publication,
            'revision_conflict', 'The ticket changed before native lifecycle dispatch.');
        });
      },
    });
    if (result.kind !== 'observed' || !['succeeded', 'failed'].includes(result.observation.phase))
      throw new IvyError('task_board_native_outcome_unresolved', 'The original native lifecycle outcome is pending; no replacement task is authorized yet.', 'unknown');
    return result.observation;
  }

  private primary(value: Primary): Primary {
    requireThat(value.namespace === 'codex' && value.kind === 'thread',
      'task_board_primary_mismatch', 'The saved native context must be a Codex thread.');
    return value;
  }

  private async archiveThread(operation: Operation, primary: Primary, archived: boolean, onlyArchived = false): Promise<boolean> {
    this.primary(primary);
    const method = archived ? 'thread/archive' : 'thread/unarchive', key = callKey(method, primary);
    let owner: NativeOwner | undefined;
    if (!await this.retained(operation, key)) {
      owner = await this.owner(primary.serviceNodeId);
      const read = await owner.read('thread/read', { threadId: primary.nativeId, includeTurns: false }, undefined, true);
      if ('result' in read.reply) {
        const thread = nativeRecord(nativeRecord(read.reply.result)['thread']);
        requireThat(thread['id'] === primary.nativeId && thread['ephemeral'] === false,
          'task_board_primary_mismatch', 'The native observation must identify the saved durable thread.');
        const path = nativeString(thread['path']);
        if (onlyArchived && !/[\\/]archived_sessions[\\/]/.test(path)) return true;
        requireThat(nativeRecord(thread['status'])['type'] !== 'active',
          'task_board_native_not_idle', 'Finish the active Codex turn before changing this ticket context.');
        if (archived && /[\\/]archived_sessions[\\/]/.test(path) ||
            !archived && /[\\/]sessions[\\/]/.test(path)) return true;
      } else if (onlyArchived || archived && missing(read.reply)) return true;
    }
    const observed = await this.call(operation, key, async () => ({
      owner: owner ?? await this.owner(primary.serviceNodeId), method, params: { threadId: primary.nativeId },
    }));
    if (observed.phase === 'succeeded') {
      if (!archived) {
        const thread = nativeRecord(observed.reply && 'result' in observed.reply ? nativeRecord(observed.reply.result)['thread'] : null);
        requireThat(thread['id'] === primary.nativeId && thread['ephemeral'] === false,
          'task_board_primary_mismatch', 'Native restoration must confirm the original durable thread.');
      }
      return true;
    }
    if (archived && missing(observed.reply)) return true;
    if (!archived) return false;
    throw new IvyError('task_board_native_archive_failed',
      observed.reply && 'error' in observed.reply ? observed.reply.error.message : 'The Codex task could not be archived.');
  }

  /** Restore only the saved context before admitting its next Run; never replace it here. */
  async restoreForExecution(operation: Operation, task: Document<'task-board/task'>): Promise<void> {
    const request = operation.value.request, primary = task.value.primaryResourceRef;
    requireThat(request.action === 'continue' && primary && primary.serviceNodeId === request.target.serviceNodeId,
      'task_board_primary_mismatch', 'Continuation must retain its saved native owner and context.');
    requireThat(await this.archiveThread(operation, primary, false, true),
      'task_board_native_restore_failed', 'The archived native context could not be restored; no replacement was created.');
  }

  private async contexts(task: Document<'task-board/task'>, includeHistory: boolean): Promise<Primary[]> {
    const unique = new Map<string, Primary>();
    const add = (primary: Primary | null) => { if (primary) unique.set(hashJson(primary), this.primary(primary)); };
    add(task.value.primaryResourceRef);
    let cursor: string | undefined;
    do {
      const page = await this.engine.store.page('task-board/run', { limit: 100,
        where: { op: 'eq', field: 'data:/taskId', value: task.pin.objectId }, ...(cursor ? { cursor } : {}) });
      for (const item of page.items) {
        const run = await this.engine.store.read('task-board/run', { objectId: item.objectId, revision: item.revision });
        requireThat(['completed', 'failed', 'cancelled'].includes(run.value.phase),
          'task_board_execution_unresolved', 'Resolve every native execution attempt before changing the ticket context.');
        add(run.value.primaryResourceRef);
      }
      cursor = page.nextCursor ?? undefined;
    } while (cursor);
    if (includeHistory) {
      cursor = undefined;
      do {
        const page: Awaited<ReturnType<TaskBoardStore['page']>> = await this.engine.store.page('task-board/history', { limit: 100,
          where: { op: 'eq', field: 'data:/taskId', value: task.pin.objectId }, ...(cursor ? { cursor } : {}) });
        for (const item of page.items) {
          const history = await this.engine.store.read('task-board/history', { objectId: item.objectId, revision: item.revision });
          if (!history.value.previousTask) continue;
          requireThat(history.value.previousTask.objectId === task.pin.objectId,
            'task_board_history_mismatch', 'Previous native contexts must belong to this ticket.');
          try {
            add((await this.engine.store.read('task-board/task', history.value.previousTask, true, true)).value.primaryResourceRef);
          } catch (error) {
            // Runs retain their execution contexts independently of pruned Task snapshots.
            if (!(error instanceof IvyError && error.code === 'revision_pruned')) throw error;
          }
        }
        cursor = page.nextCursor ?? undefined;
      } while (cursor);
    }
    return [...unique.values()];
  }

  private async plan(operation: Operation, task: Document<'task-board/task'>, previous?: TaskBoard.Run) {
    const allocationId = await scopedOperationId(this.engine.store.client, ['task-board-new-thread-plan', operation.pin.objectId]);
    const allocation = await new TaskBoardScheduler(this.engine).allocate(task, allocationId);
    if (previous) requireThat(same(allocation.target, previous.target) && allocation.workspace.canonicalCwd === previous.workspace.canonicalCwd &&
      allocation.workspace.intendedPath === previous.workspace.intendedPath, 'task_board_workspace_mismatch', 'Automatic context recovery must retain the original owner and workspace.');
    const plan = await readPlan(this.engine.store, allocation.intent), owner = await this.owner(allocation.target.serviceNodeId);
    requireThat(plan.nativeVersion === owner.target.nativeVersion, 'task_board_native_epoch_changed', 'The new native task must use its freshly allocated plan version.');
    this.engine.store.completeLocalWorkflow(allocation.intent.objectId, operation.value.createdAt);
    return { owner, plan };
  }

  private async newThread(operation: Operation, task: Document<'task-board/task'>): Promise<Primary> {
    const observed = await this.call(operation, 'new-thread', async () => {
      const { owner, plan } = await this.plan(operation, task);
      return { owner, method: 'thread/start', params: plan.threadStart };
    });
    requireThat(observed.phase === 'succeeded' && observed.reply && 'result' in observed.reply,
      'task_board_new_thread_failed', observed.reply && 'error' in observed.reply ? observed.reply.error.message : 'The new Codex task could not be started.');
    const thread = nativeRecord(nativeRecord(observed.reply.result)['thread']);
    requireThat(nativeString(thread['id']) && thread['ephemeral'] === false && thread['id'] !== task.value.primaryResourceRef?.nativeId,
      'task_board_primary_mismatch', 'The new native task must return a distinct durable thread identity.');
    return { serviceNodeId: observed.serviceNodeId, namespace: 'codex', kind: 'thread', nativeId: nativeString(thread['id']) };
  }

  private async recoverThread(operation: Operation, task: Document<'task-board/task'>): Promise<Primary | null> {
    const request = operation.value.request;
    requireThat(request.action === 'recoverThread' && operation.value.actor.source === 'scheduler' &&
      operation.value.callerPrincipalId === this.engine.settings.principalId && task.value.fields.control === 'agent' &&
      ['todo', 'waiting'].includes(task.value.workflowState) && (!task.value.waiting || task.value.waiting.reason === 'user') &&
      !task.value.blocker && same(request.run, task.value.lastRun),
      'task_board_forbidden', 'Only the scheduler can recover this exact failed, agent-controlled attempt.');
    // Preparing the first lifecycle call durably admits this exact recovery after proof verification.
    // Replay must still complete that admitted action if the closed Run's local proof later expires.
    if (!operation.value.nativeCalls?.length)
      requireThat(await unstartedMissingContext(this.engine, task), 'task_board_execution_unresolved', 'Every attempt in the missing context must be conclusively unstarted.');
    const primary = task.value.primaryResourceRef!, run = await this.engine.store.read('task-board/run', request.run);
    const observed = await this.call(operation, callKey('thread/resume', primary), async () => {
      const { owner, plan } = await this.plan(operation, task, run.value);
      return { owner, method: 'thread/resume', params: { ...plan.threadResume, threadId: primary.nativeId, excludeTurns: true } };
    });
    if (observed.phase === 'succeeded') {
      const thread = nativeRecord(observed.reply && 'result' in observed.reply ? nativeRecord(observed.reply.result)['thread'] : null);
      requireThat(thread['id'] === primary.nativeId && thread['ephemeral'] === false,
        'task_board_primary_mismatch', 'Recovery must confirm the original durable thread before reusing it.');
      return primary;
    }
    requireThat(missingNativeThread(observed.reply, primary.nativeId), 'task_board_native_resume_failed',
      'The original context was not confirmed missing; no replacement is authorized.');
    // Start the replacement and its first turn inside one Run. Resuming an empty new thread
    // can fail before Codex has persisted its rollout, even in the same native epoch.
    return null;
  }

  private async membership(operation: Operation, task: Document<'task-board/task'>, archived: boolean) {
    const store = this.engine.store;
    const current = await store.read('task-board/task', task.pin.objectId, true, true);
    const latest = (await store.read('task-board/operation', operation.pin.objectId)).value.writes
      .filter(write => write.contractKey === 'task-board/task' && write.outcome?.objectId === task.pin.objectId).at(-1)?.outcome;
    requireThat(current.pin.revision === (latest?.revision ?? task.pin.revision) || current.value.publication?.objectId === operation.pin.objectId,
      'revision_conflict', 'The ticket changed before its archive membership was saved.');
    if ((current.metadata.archivedAt !== null) === archived && current.metadata.effectivelyArchived === archived) return current.metadata;
    const result = await store.client.request('objects.archive', { objectId: task.pin.objectId, archived,
      expectedRevision: current.pin.revision, mutationId: await this.engine.operations.archiveIdentity(operation.pin.objectId) });
    requireThat(result.id === task.pin.objectId && result.parentId === store.rootObjectId && result.effectivelyArchived === archived,
      'task_board_contract_mismatch', 'Hive returned a different ticket archive outcome.');
    return result;
  }

  private async replace(operation: Operation, steps: Publication, outcome: TaskBoard.ActionOutcome,
    task: Document<'task-board/task'>, primary: Primary | null, startWork: boolean): Promise<void> {
    const at = operation.value.createdAt;
    const value: TaskBoard.Task = { ...task.value, primaryResourceRef: primary, publication: steps.pin, updatedAt: at,
      ...(startWork ? { workflowState: 'todo' as const, waiting: null, acceptedReview: null,
        fields: { ...task.value.fields, control: 'agent' as const, nextReviewAt: null } } : {}) };
    const reserved = await steps.write('task-board/task', value, { objectId: task.pin.objectId, expectedRevision: task.pin.revision });
    const request = operation.value.request;
    outcome.history = await steps.create('task-board/history', { schemaVersion: 1, taskId: task.pin.objectId,
      operationId: operation.value.operationId, actor: operation.value.actor, createdAt: at, kind: startWork && request.action !== 'recoverThread' ? 'edited' : 'recovered',
      detail: request.action === 'newThread' ? 'Work explicitly requested in a new Codex task. ' + request.reason
        : request.action === 'recoverThread' ? same(primary, task.value.primaryResourceRef)
          ? 'Automatically restored the unstarted native context; queued ticket input was retained.'
          : 'Retired the missing, conclusively unstarted native context and queued a fresh Run; the owner, workspace and queued ticket input were retained.'
        : 'The original Codex task could not be restored. A new native task was created; the ticket workflow state was retained.',
      previousTask: task.pin, previousHistory: task.value.lastHistory, fromTarget: null, toTarget: null,
      priorRun: task.value.lastRun, result: null, review: null }, 'History');
    outcome.task = await steps.write('task-board/task', { ...value, lastHistory: outcome.history, publication: null },
      { objectId: task.pin.objectId, expectedRevision: reserved.revision });
  }

  async publish(operation: Operation, steps: Publication, outcome: TaskBoard.ActionOutcome): Promise<TaskBoard.ActionOutcome> {
    const request = operation.value.request;
    requireThat(request.action === 'archive' || request.action === 'newThread' || request.action === 'recoverThread', 'task_board_native_intent_mismatch', 'Only retained ticket lifecycle requests reach this program.');
    const task = await this.engine.store.read('task-board/task', { objectId: request.taskId, revision: request.expectedRevision }, true, true);
    if (!operation.value.writes.length) {
      requireThat(task.metadata.currentRevision === request.expectedRevision, 'revision_conflict', 'Reload the ticket before changing its native context.');
      requireThat(!task.value.claim && !task.value.publication, 'task_board_execution_unresolved', 'Finish the current execution and publication before changing the ticket context.');
      requireThat(request.action === 'archive' || !task.metadata.effectivelyArchived, 'task_board_record_archived', 'Restore the ticket before requesting new work.');
      await this.engine.checks.aggregate(task.value, task.pin.objectId);
    }
    try {
      if (request.action === 'recoverThread') {
        const primary = await this.recoverThread(operation, task);
        await this.replace(operation, steps, outcome, task, primary, true);
        return outcome;
      }
      const contexts = await this.contexts(task, request.action === 'archive' && request.archived);
      if (request.action === 'newThread') {
        requireThat(this.engine.settings.scheduler.enabled,
          'task_board_scheduler_disabled', 'New Codex work requires enabled TaskBoard scheduling.');
        const primary = await this.newThread(operation, task);
        await this.replace(operation, steps, outcome, task, primary, true);
      } else {
        let replacement: Primary | null = null;
        if (request.archived) {
          for (const primary of contexts) await this.archiveThread(operation, primary, true);
        } else if (task.value.primaryResourceRef && !await this.archiveThread(operation, task.value.primaryResourceRef, false)) {
          replacement = await this.newThread(operation, task);
        }
        await this.membership(operation, task, request.archived);
        if (replacement) await this.replace(operation, steps, outcome, task, replacement, false);
        outcome.task ??= task.pin;
        outcome.archive = await this.engine.store.client.request('objects.stat', { objectId: task.pin.objectId });
        if (request.archived) this.engine.store.releaseWorkspace(task.pin.objectId);
      }
      return outcome;
    } catch (error) {
      const failure = IvyError.from(error);
      if (['service_not_ready', 'service_unavailable', 'task_board_native_unavailable', 'task_board_target_unavailable',
        'task_board_native_outcome_unresolved', 'task_board_native_epoch_changed'].includes(failure.code))
        throw new IvyError(failure.code, failure.message, 'unknown', failure.details);
      throw error;
    }
  }
}
