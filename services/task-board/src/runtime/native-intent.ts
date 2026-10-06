import { taskPrompt, taskUpdatePrompt } from './task-prompt.js';
import { hashJson } from '../../../../packages/sdk/src/node.js';
import { requireThat } from '../../../../packages/sdk/src/node.js';
import { validateTaskBoard } from '../../../../packages/sdk/src/node.js';
import type { TaskBoard, Wire } from '../../../../packages/sdk/src/node.js';
import type { NativeEvidenceIdentity } from './evidence.js';
import type { TaskBoardStore } from './store.js';
import { readPlan } from './native-plan.js';
import { checkedNativeContract } from '../../../../packages/sdk/src/node.js';
import { validateNativeInvocation } from '../../../../packages/sdk/src/native-plan.js';
import type { NativeInvocation } from '../../../../packages/sdk/src/native-plan.js';
import { readNativeParameters, saveNativeParameters } from '../../../../packages/sdk/src/native-parameters.js';
import { mutation } from './store.js';

export type NativeSlot = keyof TaskBoard.Run['calls'];
export const nativeRecord = (value: unknown): Record<string, Wire.Json> => value !== null && typeof value === 'object' && !Array.isArray(value) ? value as Record<string, Wire.Json> : {};
export const nativeArray = (value: unknown): Wire.Json[] => Array.isArray(value) ? value : [];
export const nativeString = (value: unknown): string => typeof value === 'string' ? value : '';

/** Materialize execution from the immutable plan; resume replies keep history paginated. */
export async function intendedCall(store: TaskBoardStore, run: TaskBoard.Run, slot: NativeSlot, origin: TaskBoard.NativeCallOrigin = { kind: 'initial' }): Promise<{ request: NativeInvocation; expectedDefinitionHash: string }> {
  validateTaskBoard('NativeCallOrigin', origin);
  const plan = run.plan;
  let method: TaskBoard.NativeRequest['method'], params: unknown, definition: keyof TaskBoard.NativePlan['definitions'] = 'turnStart';
  if (origin.kind === 'steer') {
    requireThat(slot === 'steer' && run.primaryResourceRef && run.turnId && run.nativeEpoch && !run.cancellation,
      'task_board_native_order', 'Steering requires the exact active native turn.');
    const task = await store.read('task-board/task', run.taskId);
    method = 'turn/steer'; params = { threadId: run.primaryResourceRef.nativeId, expectedTurnId: run.turnId,
      input: [{ type: 'text', text: taskUpdatePrompt(task.value, run.owner.serviceNodeId), text_elements: [] }] };
  } else if (origin.kind === 'reattach') {
    const primary = run.primaryResourceRef;
    requireThat(slot === 'thread' && primary?.namespace === 'codex' && primary.kind === 'thread' && primary.serviceNodeId === run.target.serviceNodeId,
      'task_board_primary_mismatch', 'Reattachment requires the saved primary thread on the selected native owner.');
    method = 'thread/resume'; definition = 'threadResume'; params = { ...plan.threadResume, threadId: primary.nativeId, excludeTurns: true };
  } else if (slot === 'thread') {
    const initialTask = await store.read('task-board/task', { objectId: run.taskId, revision: run.originalRequest.expectedRevision });
    const primary = initialTask.value.primaryResourceRef;
    requireThat(!primary || primary.namespace === 'codex' && primary.kind === 'thread' && primary.serviceNodeId === run.target.serviceNodeId, 'task_board_primary_mismatch', 'The original primary must belong to the selected native owner.');
    method = primary ? 'thread/resume' : 'thread/start'; definition = primary ? 'threadResume' : 'threadStart';
    params = primary ? { ...plan.threadResume, threadId: primary.nativeId, excludeTurns: true } : plan.threadStart;
  } else if (slot === 'turn') {
    requireThat(run.primaryResourceRef && run.nativeEpoch, 'task_board_primary_required', 'Turn preparation requires the saved primary and native epoch.');
    const task = await store.read('task-board/task', { objectId: run.taskId, revision: run.originalRequest.expectedRevision });
    const taskInput = { type: 'text', text: taskPrompt(task.value, run.owner.serviceNodeId, run.originalRequest.action === 'continue' && run.originalRequest.coordinationOnly), text_elements: [] };
    method = 'turn/start'; definition = 'turnStart'; params = { ...plan.turnStart, threadId: run.primaryResourceRef.nativeId,
      input: run.owner.source === 'scheduler' ? [taskInput] : [...nativeArray(plan.turnStart.input), taskInput] };
  } else {
    requireThat(run.primaryResourceRef && run.turnId && run.cancellation, 'task_board_cancellation_required', 'Interruption requires the exact known turn and saved cancellation intent.');
    method = 'turn/interrupt'; definition = 'turnInterrupt'; params = { threadId: run.primaryResourceRef.nativeId, turnId: run.turnId };
  }
  const request = { nativeVersion: plan.nativeVersion, catalogSourceHash: plan.catalogSourceHash, method, params } as NativeInvocation;
  validateNativeInvocation(checkedNativeContract(plan.nativeVersion, { sourceHash: plan.catalogSourceHash }).contract, request);
  return { request, expectedDefinitionHash: slot === 'steer' ? hashJson(checkedNativeContract(plan.nativeVersion, { sourceHash: plan.catalogSourceHash }).definitions.get(method)) : plan.definitions[definition] };
}

export async function retainNativeRequest(store: TaskBoardStore, request: NativeInvocation, operationId: string): Promise<TaskBoard.NativeRequest> {
  const contract = checkedNativeContract(request.nativeVersion, { sourceHash: request.catalogSourceHash }).contract; validateNativeInvocation(contract, request);
  const params = await saveNativeParameters(store.client, contract, { method: request.method, params: request.params,
    parentId: store.rootObjectId, mutationId: mutation(operationId, 'native-parameters') });
  return { ...request, params };
}
export async function resolveNativeRequest(store: TaskBoardStore, request: TaskBoard.NativeRequest): Promise<NativeInvocation> {
  validateTaskBoard('NativeRequest', request); const contract = checkedNativeContract(request.nativeVersion, { sourceHash: request.catalogSourceHash }).contract;
  const params = await readNativeParameters(store.client, contract, { method: request.method, ref: request.params, parentId: store.rootObjectId });
  const result = { ...request, params }; validateNativeInvocation(contract, result); return result;
}
export const evidenceIdentity = async (store: TaskBoardStore, run: TaskBoard.Run, call: TaskBoard.NativeCall, principalId: string): Promise<NativeEvidenceIdentity> => ({
  callerPrincipalId: principalId, operationId: call.operationId, serviceNodeId: run.target.serviceNodeId,
  nativeVersion: call.request.nativeVersion, method: call.request.method, params: (await resolveNativeRequest(store, call.request)).params,
});
