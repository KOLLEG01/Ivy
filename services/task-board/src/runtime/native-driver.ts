import { hashJson } from '../../../../packages/sdk/src/node.js';
import { IvyError, requireThat } from '../../../../packages/sdk/src/node.js';
import { validateAgent } from '../../../../packages/sdk/src/node.js';
import type { Agent, TaskBoard, Wire } from '../../../../packages/sdk/src/node.js';
import { scopedOperationId, serviceTools } from '../../../../packages/sdk/src/client.js';
import { NativeOwner } from '../../../../packages/sdk/src/native-owner.js';
import { reconcileNativeOperation } from '../../../../packages/sdk/src/native-operation.js';
import { validateNativeRead, validateNativeStatus } from '../../../../packages/sdk/src/native-evidence.js';
import { nativeThreadState } from '../../../../packages/sdk/src/native-observations.js';
import { same } from './checks.js';
import { TaskBoardEvidence, catalogFor } from './evidence.js';
import { evidenceMaximumBytes } from './evidence-bytes.js';
import { TaskBoardEngine } from './engine.js';
import { evidenceIdentity, intendedCall, retainNativeRequest, resolveNativeRequest } from './native-intent.js';
import type { NativeSlot } from './native-intent.js';
import { finishedRun } from './native-publication.js';
import { mutation } from './store.js';
import type { Document } from './store.js';

/** Executes only attached retained calls. Scheduling and complete turn inspection are separate. */
export class TaskBoardNativeDriver {
  constructor(readonly engine: TaskBoardEngine) {}
  private async current(runId: string) {
    await this.engine.verifyOwner();
    const run = await this.engine.store.read('task-board/run', runId), task = await this.engine.store.read('task-board/task', run.value.taskId);
    requireThat(!finishedRun(run.value) && !task.value.publication, 'task_board_native_order', 'Native execution requires an unfinished Run and no pending task publication.');
    const active = task.value.claim?.run?.objectId === runId;
    requireThat(!active || same(task.value.claim!.run, run.pin), 'revision_conflict', 'Native execution requires the exact current claimed Run revision.');
    return { run, task, active };
  }
  private async update(run: Document<'task-board/run'>, task: Document<'task-board/task'>, change: TaskBoard.NativeUpdateRequest['change']) {
    return this.engine.nativeUpdate({ action: 'nativeUpdate', operationId: mutation(run.pin.objectId, 'native-update:' + hashJson({ task: task.pin, run: run.pin, change })),
      taskId: task.pin.objectId, expectedRevision: task.pin.revision, run: run.pin, change });
  }
  async prepare(runId: string, slot: NativeSlot, origin: TaskBoard.NativeCallOrigin = { kind: 'initial' }): Promise<TaskBoard.ObjectPin> {
    const { run, task, active } = await this.current(runId), store = this.engine.store;
    requireThat(slot === 'interrupt' ? !!run.value.cancellation : active && !run.value.cancellation, 'task_board_claim_inactive', 'Only an active uncancelled claim may prepare execution; interruption requires cancellation.');
    const operationId = await scopedOperationId(store.client, ['task-board-native-call', run.value.operationId, slot, origin]), name = 'Native call ' + operationId;
    let call = await store.named('task-board/native-call', name);
    if (!call) {
      const intent = await intendedCall(store, run.value, slot, origin);
      const request = await retainNativeRequest(store, intent.request, operationId);
      const value: TaskBoard.NativeCall = { ...intent, request, origin, operationId, state: 'prepared', preparedAt: new Date().toISOString(), observedAt: null, ownerPhase: null, epoch: null, evidence: null, code: null };
      try {
        const pin = await store.write('task-board/native-call', value, mutation(operationId, 'prepare'), { create: { parentId: store.rootObjectId, name } });
        call = await store.read('task-board/native-call', pin);
      } catch (error) {
        if (!(error instanceof IvyError && ['mutation_conflict', 'revision_conflict'].includes(error.code))) throw error;
        call = await store.named('task-board/native-call', name); requireThat(call, 'task_board_identity_conflict', 'The competing NativeCall must remain available under its original identity.');
      }
    }
    if (run.value.calls[slot]?.objectId === call.pin.objectId) return call.pin;
    await this.update(run, task, origin.kind === 'reattach' ? { kind: 'reattachCall', call: call.pin } : { kind: 'attachCall', slot, call: call.pin });
    return call.pin;
  }
  private async management(node: string, name: string, args: Wire.Json): Promise<Wire.Json> {
    return serviceTools(this.engine.store.client, node, [{ namespace: 'agent', interfaceVersion: '1.0.0' }]).read('agent.' + name, args);
  }
  private nativeOwner(run: TaskBoard.Run, call: TaskBoard.NativeCall): NativeOwner {
    const source = catalogFor(call.request.nativeVersion, { sourceHash: call.request.catalogSourceHash });
    return new NativeOwner(this.engine.store.client, this.engine.settings.principalId, { serviceNodeId: run.target.serviceNodeId,
      hostId: run.target.hostId, nativeVersion: call.request.nativeVersion, nativeExecutableHash: source.catalog.nativeExecutableHash, catalogHash: source.catalogHash });
  }
  async status(run: TaskBoard.Run, call: TaskBoard.NativeCall): Promise<Agent.Status> {
    const value = await this.management(run.target.serviceNodeId, 'status', {});
    validateNativeStatus(value, this.nativeOwner(run, call).target, 'task_board_native_unavailable'); return value;
  }
  /** A resume has no user input. A replacement owner may attach the same saved thread
   * after the old receipt became uncertain, but only after reading that exact context. */
  async recoverUncertainResume(runId: string): Promise<boolean> {
    const { run, active } = await this.current(runId), pin = run.value.calls.thread;
    if (!active || run.value.cancellation || run.value.turnId || !pin || !run.value.primaryResourceRef) return false;
    if (run.value.calls.turn) {
      const turn = await this.engine.store.read('task-board/native-call', run.value.calls.turn.objectId);
      if (turn.pin.revision !== run.value.calls.turn.revision || turn.value.state !== 'prepared' ||
          turn.value.ownerPhase !== null || turn.value.evidence !== null) return false;
    }
    const call = await this.engine.store.read('task-board/native-call', pin);
    if (call.value.request.method !== 'thread/resume' || call.value.ownerPhase !== 'outcome_unknown' ||
        !call.value.evidence || !call.value.epoch || call.pin.revision !== pin.revision) return false;
    const primary = run.value.primaryResourceRef;
    requireThat(primary.namespace === 'codex' && primary.kind === 'thread' && primary.serviceNodeId === run.value.target.serviceNodeId &&
      (await resolveNativeRequest(this.engine.store, call.value.request)).params['threadId'] === primary.nativeId,
      'task_board_primary_mismatch', 'The uncertain resume must identify the saved native context.');
    const status = await this.status(run.value, call.value);
    if (status.epoch === call.value.epoch) return false;
    const read = await this.nativeOwner(run.value, call.value).read('thread/read',
      { threadId: primary.nativeId, includeTurns: false }, status.epoch!, true);
    requireThat('result' in read.reply, 'task_board_native_read_failed',
      'The saved native context is unavailable to the replacement owner.');
    const thread = (read.reply.result as { thread?: { id?: string; ephemeral?: boolean } })?.thread;
    requireThat(thread?.id === primary.nativeId && thread.ephemeral === false,
      'task_board_primary_mismatch', 'Recovery read a different native context.');
    await this.prepare(runId, 'thread', { kind: 'reattach', predecessor: pin, preparedEpoch: status.epoch! });
    return true;
  }
  async advance(runId: string, slot: NativeSlot): Promise<TaskBoard.ActionOutcome | null> {
    const { run, task, active } = await this.current(runId), store = this.engine.store, pin = run.value.calls[slot];
    requireThat(pin, 'task_board_native_order', 'Dispatch requires an attached retained NativeCall.');
    const call = await store.read('task-board/native-call', pin.objectId);
    const nativeRequest = await resolveNativeRequest(store, call.value.request);
    // A committed Call observation may have lost its write/publication response. Publish that
    // exact saved revision before asking the native owner for anything else.
    if (call.pin.revision > pin.revision) return this.update(run, task, { kind: 'observeCall', slot, call: call.pin });
    requireThat(!['succeeded', 'failed'].includes(call.value.ownerPhase ?? ''), 'task_board_native_order', 'The attached call already has its conclusive original observation.');
    const evidence = new TaskBoardEvidence(store), expected = await evidenceIdentity(store, run.value, call.value, this.engine.settings.principalId);
    const owner = this.nativeOwner(run.value, call.value);
    const reconciled = await reconcileNativeOperation<TaskBoard.ActionOutcome | null>({ owner,
      call: { operationId: call.value.operationId, method: call.value.request.method, params: nativeRequest.params, definitionHash: call.value.expectedDefinitionHash },
      absenceConflictCode: 'task_board_native_absence_conflict',
      journal: { current: null, previous: call.value.evidence ? await evidence.read(call.pin, call.value.evidence, expected) : null,
        wasObserved: call.value.state !== 'prepared' || call.value.ownerPhase !== null, retain: async observed => {
          const artifact = await evidence.save(call.pin, observed, expected);
          const next: TaskBoard.NativeCall = { ...call.value, state: observed.phase === 'outcome_unknown' ? 'outcome_unknown' : 'observed', ownerPhase: observed.phase,
            observedAt: observed.updatedAt, epoch: observed.epoch, evidence: artifact, code: observed.code };
          const saved = await store.write('task-board/native-call', next, mutation(call.pin.objectId, 'observe:' + hashJson(observed)), { objectId: call.pin.objectId, expectedRevision: call.pin.revision });
          return this.update(run, task, { kind: 'observeCall', slot, call: saved });
        } },
      onAbsent: async (original, absence) => {
        const status = await this.status(run.value, call.value);
        requireThat(absence.epoch === status.epoch, 'task_board_native_epoch_changed', 'Absence and current native status must come from the same epoch.');
        const primaryFailed = slot === 'turn' && run.value.calls.thread && (await store.read('task-board/native-call', run.value.calls.thread)).value.ownerPhase === 'failed';
        if ((!active || run.value.cancellation || primaryFailed || slot === 'steer' && status.epoch !== run.value.nativeEpoch) && slot !== 'interrupt') {
          const prevented = await owner.management('prevent', { operationId: call.value.operationId,
            nativeVersion: call.value.request.nativeVersion, method: call.value.request.method, params: nativeRequest.params,
            expectedDefinitionHash: call.value.expectedDefinitionHash }, call.value.operationId);
          validateAgent('Operation', prevented); return prevented as Agent.Operation;
        } else {
          if (slot === 'turn') {
            requireThat(status.epoch === run.value.nativeEpoch, 'task_board_reattach_required', 'The saved primary must be confirmed in the current native epoch before turn dispatch.');
            requireThat(run.value.calls.thread, 'task_board_native_order', 'A turn needs its retained thread receipt.');
            const threadCall = await store.read('task-board/native-call', run.value.calls.thread);
            requireThat(threadCall.value.ownerPhase === 'succeeded' && threadCall.value.epoch === status.epoch, 'task_board_native_order', 'The latest primary attachment must be confirmed in this native epoch before dispatch.');
            const params = { threadId: run.value.primaryResourceRef!.nativeId, includeTurns: false };
            const snapshot = await this.management(run.value.target.serviceNodeId, 'read', { nativeVersion: call.value.request.nativeVersion, method: 'thread/read', params });
            validateNativeRead(snapshot, { ...owner.target, callerPrincipalId: this.engine.settings.principalId, epoch: status.epoch!, method: 'thread/read', params }, 'task_board_native_unavailable');
            requireThat('result' in snapshot.reply, 'task_board_native_unavailable', 'Turn dispatch requires a successful fresh native primary observation.');
            const { thread, canStartTurn } = nativeThreadState(catalogFor(snapshot.nativeVersion, { catalogHash: snapshot.catalogHash }).contract, snapshot.reply.result);
            requireThat(thread['id'] === run.value.primaryResourceRef!.nativeId && thread['ephemeral'] === false && canStartTurn,
              'task_board_native_not_idle', 'The native primary must be currently attached, accept input and have no active turn before its turn starts.');
          }
          if (slot === 'interrupt') requireThat(status.epoch === run.value.nativeEpoch, 'task_board_native_epoch_changed', 'An old turn cannot be interrupted as if the replacement owner controlled it.');
          return owner.dispatch(original, async () => {
            const fresh = await this.current(runId);
            requireThat(same(fresh.run.pin, run.pin) && same(fresh.task.pin, task.pin), 'revision_conflict', 'Task or Run changed before native dispatch; reconcile it before sending.');
            await this.engine.verifyOwner();
          });
        }
      } });
    requireThat(reconciled.kind === 'observed', 'task_board_native_outcome_unavailable', 'No original native operation was observed after dispatch; do not invent a receipt.');
    return reconciled.value;
  }
}
