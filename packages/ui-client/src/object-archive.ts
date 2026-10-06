import { computed, ref } from 'vue';
import { callBound, discover, IvyError, newOperationId } from '../../sdk/src/client.js';
import type { Result, RpcClient } from '../../sdk/src/client.js';
import type { TaskBoard, Wire } from '../../sdk/src/client.js';

interface PendingArchive { objectId: string; archived: boolean; mutationId: string; call?: Wire.ToolCall }

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
      return { qualifiedName: binding.qualifiedName, serviceNodeId: node.serviceNodeId, expectedDefinitionHash: binding.definitionHash,
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
  try {
    const saved = JSON.parse(sessionStorage.getItem(key) ?? 'null') as Partial<PendingArchive> | null;
    if (saved && typeof saved.objectId === 'string' && typeof saved.archived === 'boolean' && typeof saved.mutationId === 'string') pending.value = saved as PendingArchive;
  } catch { error.value = 'The original archive action cannot be read from this tab.'; }
  const persist = () => {
    try { pending.value ? sessionStorage.setItem(key, JSON.stringify(pending.value)) : sessionStorage.removeItem(key); }
    catch { throw new Error('The archive action could not be retained in this tab.'); }
  };
  const run = async (objectId: string, archived: boolean) => {
    if (busy.value || error.value || pending.value && (pending.value.objectId !== objectId || pending.value.archived !== archived)) return null;
    busy.value = true;
    try {
      pending.value ??= { objectId, archived, mutationId: await newOperationId(client) }; persist();
      if (!pending.value.call) {
        const call = await taskArchiveCall(client, pending.value);
        if (call) pending.value.call = call;
        persist();
      }
      const result = pending.value.call
        ? await client.request('tools.call', pending.value.call)
        : await client.request('objects.archive', { objectId: pending.value.objectId, archived: pending.value.archived, mutationId: pending.value.mutationId });
      pending.value = null; persist(); return result;
    } catch (cause) {
      const failure = IvyError.from(cause);
      if (failure.outcome === 'not_executed') { pending.value = null; try { persist(); } catch { /* The actionable error below remains visible. */ } }
      error.value = cause instanceof Error ? cause.message : 'The archive action could not be confirmed.';
      return null;
    } finally { busy.value = false; }
  };
  return { pending, busy, error, run, retrying: computed(() => !!pending.value), clearError: () => { error.value = null; } };
}
