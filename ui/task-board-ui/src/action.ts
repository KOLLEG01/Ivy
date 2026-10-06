import { computed, ref } from 'vue';
import type { Ref } from 'vue';
import { discover, IvyError, newOperationId } from '../../../packages/sdk/src/client.js';
import type { TaskBoard, Wire } from './runtime';
import { base, client } from './runtime';
import { nativeRead } from '../../../packages/ui-client/src/native';
import { canonical } from '../../../packages/sdk/src/client.js';
import { hashJson } from '../../../packages/ui-client/src/hash';
type WithoutId<T> = T extends unknown ? Omit<T, 'operationId'> : never;
export type DraftRequest = WithoutId<TaskBoard.ActionInput | TaskBoard.ArchiveRequest>;
export interface SavedAction {
  label: string; callerPrincipalId: string; principalId: string; rootObjectId: string | null; call: Wire.ToolCall;
  phase: 'prepared' | 'unknown' | 'pending' | 'failed' | 'succeeded'; detail: string; outcome: TaskBoard.ActionOutcome | null;
}
export function useAction(workspace: () => TaskBoard.WorkspaceInfo, scope: string) {
  const key = 'ivy:task-board:action:' + base.href + workspace().serviceNodeId + ':' + scope;
  const saved = ref(null) as Ref<SavedAction | null>, busy = ref(false), error = ref<string | null>(null), storageError = ref(false);
  try { const raw = sessionStorage.getItem(key); if (raw) { const value = JSON.parse(raw) as SavedAction;
    if (!value.call?.operationId || !value.callerPrincipalId || typeof value.principalId !== 'string') throw new Error('Invalid retained action'); saved.value = value; } }
  catch { storageError.value = true; error.value = 'The original action cannot be read from this tab. Restore browser storage before sending.'; }
  const identityMatches = () => !saved.value || saved.value.callerPrincipalId === workspace().callerPrincipalId && saved.value.principalId === workspace().principalId && saved.value.rootObjectId === workspace().rootObjectId && saved.value.call.serviceNodeId === workspace().serviceNodeId;
  const persist = () => { try { sessionStorage.setItem(key, JSON.stringify(saved.value)); } catch { storageError.value = true; throw new Error('The original action could not be retained. Further sends are paused.'); } };
  const checkCurrentIdentity = async () => {
    const current = await nativeRead(client, workspace().serviceNodeId, 'task-board.workspace', {}) as TaskBoard.WorkspaceInfo;
    if (!saved.value || current.callerPrincipalId !== saved.value.callerPrincipalId || current.principalId !== saved.value.principalId || current.rootObjectId !== saved.value.rootObjectId || current.serviceNodeId !== saved.value.call.serviceNodeId)
      throw new Error('The current caller or workflow owner changed. The original action is retained without being sent.');
  };
  const readOperation = async () => {
    if (!saved.value) throw new Error('The original action is unavailable.');
    const operation = await nativeRead(client, workspace().serviceNodeId, 'task-board.operation', { operationId: saved.value.call.operationId! }) as TaskBoard.Operation;
    const matches = operation.request.action === 'savePlan'
      ? operation.request.draftHash === await hashJson(saved.value.call.arguments)
      : canonical(operation.request) === canonical(saved.value.call.arguments);
    if (operation.callerPrincipalId !== saved.value.callerPrincipalId || operation.operationId !== saved.value.call.operationId || !matches) throw new Error('The operation differs from the retained caller or original request.');
    return operation;
  };
  const execute = async () => {
    if (!saved.value || busy.value || !identityMatches()) return;
    busy.value = true; error.value = null;
    try {
      await checkCurrentIdentity();
      saved.value.phase = 'unknown'; saved.value.detail = 'Waiting for the original workflow operation.'; persist();
      let value = await client.request('tools.call', saved.value.call) as TaskBoard.ActionOutcome;
      // Archive returns Object metadata; its workflow receipt is retained under the same operation.
      if ((saved.value.call.arguments as { action: string }).action === 'archive') {
        const operation = await readOperation();
        if (operation.phase !== 'succeeded' || !operation.outcome) throw new Error('The original archive action has not finished.');
        value = operation.outcome;
      }
      if (value.operationId !== saved.value.call.operationId) throw new Error('The response does not identify the original action.');
      saved.value.outcome = value; saved.value.phase = 'succeeded'; saved.value.detail = 'The workflow action was saved.'; persist();
    } catch (cause) {
      const failure = IvyError.from(cause);
      saved.value.phase = failure.outcome === 'unknown' || !(cause instanceof IvyError) ? 'unknown' : 'failed';
      saved.value.detail = `${failure.message} (${failure.code}; ${failure.outcome})`;
      try { persist(); } catch (problem) { error.value = String(problem); }
    } finally { busy.value = false; }
  };
  const start = async (label: string, request: DraftRequest) => {
    if (busy.value || error.value || !identityMatches() || saved.value && !['succeeded', 'failed'].includes(saved.value.phase)) return null;
    busy.value = true;
    try {
      const current = workspace(), binding = await discover(client, 'task-board.' + request.action, { serviceNodeId: current.serviceNodeId }), operationId = await newOperationId(client);
      saved.value = { label, callerPrincipalId: current.callerPrincipalId, principalId: current.principalId, rootObjectId: current.rootObjectId, phase: 'prepared', detail: '', outcome: null,
        call: { qualifiedName: binding.qualifiedName, serviceNodeId: binding.serviceNodeId, expectedDefinitionHash: binding.definitionHash, operationId,
          arguments: { ...request, operationId, expectedWorkspace: { principalId: current.principalId, rootObjectId: current.rootObjectId, callerPrincipalId: current.callerPrincipalId } } as Wire.Json } };
      persist(); busy.value = false; await execute(); return saved.value.phase === 'succeeded' ? saved.value.outcome : null;
    } catch (cause) { busy.value = false; error.value = cause instanceof Error ? cause.message : 'The action could not be prepared.'; return null; }
  };
  const reconcile = async () => {
    if (!saved.value || busy.value || !identityMatches()) return;
    busy.value = true; error.value = null;
    try {
      await checkCurrentIdentity();
      const operation = await readOperation();
      saved.value.phase = operation.phase === 'succeeded' ? 'succeeded' : operation.phase === 'failed' ? 'failed' : 'pending';
      saved.value.outcome = operation.outcome; saved.value.detail = operation.error?.message ?? 'Original operation: ' + operation.phase + '.'; persist();
    } catch (cause) { error.value = cause instanceof Error ? cause.message : 'The original outcome is unavailable.'; }
    finally { busy.value = false; }
  };
  const canRetryPreparation = computed(() => !!error.value && !storageError.value && !busy.value && (!saved.value || ['succeeded', 'failed'].includes(saved.value.phase)));
  return { saved, error, busy, start, reconcile, replay: execute, canRetryPreparation,
    retryPreparation: () => { if (canRetryPreparation.value) error.value = null; },
    locked: computed(() => busy.value || !!error.value || !identityMatches() || !!saved.value && !['succeeded', 'failed'].includes(saved.value.phase)),
    identityMatches: computed(identityMatches) };
}
