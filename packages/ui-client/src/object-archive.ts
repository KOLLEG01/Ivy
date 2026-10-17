import { computed, ref } from 'vue';
import { useRemote } from '@ivy/ui';
import { callBound, canonical, discover, IvyError, newOperationId } from '../../sdk/src/client.js';
import type { Result, RpcClient } from '../../sdk/src/client.js';
import type { TaskBoard, Wire } from '../../sdk/src/client.js';

export interface PendingArchive { objectId: string; archived: boolean; mutationId: string; call?: Wire.ToolCall }
type ArchiveOutcome = { phase: 'succeeded'; archive: Result<'objects.archive'>; operation?: TaskBoard.Operation }
  | { phase: 'failed'; message: string; operation: TaskBoard.Operation };

const matchesArchive = (object: Result<'objects.stat'>, pending: PendingArchive) =>
  object.id === pending.objectId && (object.archivedAt !== null) === pending.archived && (!pending.call || object.effectivelyArchived === pending.archived);

/** Read-only recovery: a missing receipt alone never proves that an action finished. */
export async function readArchiveOutcome(client: RpcClient, pending: PendingArchive): Promise<ArchiveOutcome | null> {
  const call = pending.call;
  if (call) {
    const request = call.arguments as TaskBoard.ArchiveRequest, expected = request?.expectedWorkspace;
    if (!call.serviceNodeId || call.operationId !== pending.mutationId || request?.operationId !== pending.mutationId ||
        request.action !== 'archive' || request.taskId !== pending.objectId || request.archived !== pending.archived || !expected)
      throw new Error('The original archive identity is incomplete.');
    const options = { expectedCallerPrincipalId: expected.callerPrincipalId };
    const workspace = await callBound(client, await discover(client, 'task-board.workspace', { serviceNodeId: call.serviceNodeId }), {}, undefined, options) as TaskBoard.WorkspaceInfo;
    if (workspace.callerPrincipalId !== expected.callerPrincipalId || workspace.principalId !== expected.principalId || workspace.rootObjectId !== expected.rootObjectId)
      throw new Error('The original archive belongs to another caller or workspace.');
    let operation: TaskBoard.Operation;
    try {
      operation = await callBound(client, await discover(client, 'task-board.operation', { serviceNodeId: call.serviceNodeId }),
        { operationId: call.operationId }, undefined, options) as TaskBoard.Operation;
    } catch (cause) {
      // Only the owner's eviction guard proves this workflow is terminal. A not-found
      // or unavailable operation can still be awaiting acceptance or resolution.
      if (!(cause instanceof IvyError) || cause.code !== 'operation_expired') throw cause;
      const object = await client.request('objects.stat', { objectId: pending.objectId });
      if (object.parentId === expected.rootObjectId && matchesArchive(object, pending)) return { phase: 'succeeded', archive: object };
      return null;
    }
    if (operation.operationId !== call.operationId || operation.callerPrincipalId !== expected.callerPrincipalId ||
        canonical(operation.request) !== canonical(call.arguments))
      throw new Error('The archive receipt differs from the original caller or request.');
    if (operation.phase === 'succeeded' && operation.outcome?.archive && matchesArchive(operation.outcome.archive, pending))
      return { phase: 'succeeded', archive: operation.outcome.archive, operation };
    if (operation.phase === 'failed') return { phase: 'failed', message: operation.error?.message ?? 'The archive action failed.', operation };
    return null;
  }
  const object = await client.request('objects.stat', { objectId: pending.objectId });
  return matchesArchive(object, pending) ? { phase: 'succeeded', archive: object } : null;
}

/** Service-owned tickets use their domain lifecycle from every shared archive control. */
async function taskArchiveCall(client: RpcClient, pending: PendingArchive): Promise<Wire.ToolCall | undefined> {
  const object = await client.request('objects.stat', { objectId: pending.objectId });
  if (object.contractKey !== 'task-board/task') return undefined;
  let cursor: string | null = null;
  do {
    const page: Result<'serviceNodes.list'> = await client.request('serviceNodes.list', { serviceName: 'task-board', limit: 200, ...(cursor ? { cursor } : {}) });
    for (const node of page.items.filter(node => node.connected && node.synced && node.ready)) {
      const workspace = await callBound(client, await discover(client, 'task-board.workspace', { serviceNodeId: node.serviceNodeId }), {}) as TaskBoard.WorkspaceInfo;
      if (workspace.rootObjectId !== object.parentId) continue;
      const binding = await discover(client, 'task-board.archive', { serviceNodeId: node.serviceNodeId });
      return { qualifiedName: binding.qualifiedName, serviceNodeId: node.serviceNodeId, expectedDefinitionHash: binding.definitionHash, expectedCallerPrincipalId: workspace.callerPrincipalId,
        operationId: pending.mutationId, arguments: { action: 'archive', operationId: pending.mutationId,
          taskId: object.id, expectedRevision: object.currentRevision, archived: pending.archived,
          expectedWorkspace: { callerPrincipalId: workspace.callerPrincipalId, principalId: workspace.principalId, rootObjectId: workspace.rootObjectId } } };
    }
    cursor = page.nextCursor;
  } while (cursor);
  throw new IvyError('service_unavailable', 'The owning TaskBoard must be available to archive or restore this ticket.');
}

export function useObjectArchive(client: RpcClient, key: string) {
  const pending = ref<PendingArchive | null>(null), busy = ref(false), error = ref<string | null>(null);
  let storageBlocked = false;
  try {
    const saved = JSON.parse(sessionStorage.getItem(key) ?? 'null') as Partial<PendingArchive> | null;
    if (saved) {
      if (typeof saved.objectId !== 'string' || typeof saved.archived !== 'boolean' || typeof saved.mutationId !== 'string') throw new Error('Invalid retained archive.');
      pending.value = saved as PendingArchive;
    }
  } catch { storageBlocked = true; error.value = 'The original archive action cannot be read from this tab.'; }
  const persist = () => {
    try { pending.value ? sessionStorage.setItem(key, JSON.stringify(pending.value)) : sessionStorage.removeItem(key); }
    catch { storageBlocked = true; throw new Error('The archive action could not be retained in this tab.'); }
  };
  const clearPending = () => {
    // Keep the in-memory identity too if browser storage cannot acknowledge removal.
    try { sessionStorage.removeItem(key); }
    catch { storageBlocked = true; throw new Error('The archive result could not be retained in this tab.'); }
    pending.value = null;
  };
  const settle = async (original: PendingArchive) => {
    const outcome = await readArchiveOutcome(client, original);
    if (!outcome || pending.value !== original) return null;
    clearPending();
    error.value = outcome.phase === 'failed' ? outcome.message : null;
    return outcome.phase === 'succeeded' ? outcome.archive : null;
  };
  const reconcile = async () => {
    if (!pending.value || busy.value || storageBlocked) return null;
    busy.value = true; error.value = null;
    try { return await settle(pending.value); }
    catch (cause) { error.value = cause instanceof Error ? cause.message : 'The archive action could not be confirmed.'; return null; }
    finally { busy.value = false; }
  };
  // Recover on mount, reconnection and return to this tab, with a bounded fallback
  // while a lost response is unresolved. Idle controls do not make any requests.
  useRemote(reconcile, 15000);
  const run = async (objectId: string, archived: boolean) => {
    if (busy.value || storageBlocked || error.value || pending.value && (pending.value.objectId !== objectId || pending.value.archived !== archived)) return null;
    const replay = !!pending.value;
    busy.value = true;
    try {
      pending.value ??= { objectId, archived, mutationId: await newOperationId(client) }; persist();
      if (!pending.value.call) {
        const call = await taskArchiveCall(client, pending.value);
        if (call) pending.value.call = call;
        persist();
      }
      const result = (pending.value.call
        ? await client.request('tools.call', pending.value.call)
        : await client.request('objects.archive', { objectId: pending.value.objectId, archived: pending.value.archived, mutationId: pending.value.mutationId })) as Result<'objects.archive'>;
      if (!matchesArchive(result, pending.value)) throw new Error('The result does not confirm the original archive action.');
      clearPending(); error.value = null; return result;
    } catch (cause) {
      const failure = IvyError.from(cause);
      // A lost response can follow a completed archive. Read the original receipt
      // before leaving the control blocked or asking the user to retry.
      if (pending.value && !storageBlocked) {
        try {
          const result = await settle(pending.value);
          if (!pending.value) return result;
        } catch { /* Keep the original request when its outcome is still unavailable. */ }
      }
      // A rejected retry says nothing about an earlier lost response. Keep its
      // identity until the owner confirms failure/success or safely retired history.
      if (!replay && failure.outcome === 'not_executed' && !storageBlocked) {
        try { clearPending(); } catch (problem) { error.value = problem instanceof Error ? problem.message : 'The archive result could not be retained.'; return null; }
      }
      error.value = cause instanceof Error ? cause.message : 'The archive action could not be confirmed.';
      return null;
    } finally { busy.value = false; }
  };
  return { pending, busy, error, run, reconcile, retrying: computed(() => !!pending.value), clearError: () => { if (!storageBlocked) error.value = null; } };
}
