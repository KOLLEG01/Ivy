import { canonical, IvyError } from '../../sdk/src/client.js';
import type { Agent, RpcClient } from '../../sdk/src/client.js';
import { validateAgent } from '../../contracts/src/agent-validation.js';
import { nativeAccountRead, nativeMethod } from './native-backends.js';
import { enumStrings, nativeRead, optionalTool, record, schemaNode, supportsFields, text } from './native.js';

export interface NativeThreadListItem {
  id: string;
  name: string;
  preview: string;
  cwd: string;
}
type Owner = { serviceNodeId: string; backend?: Agent.BackendIdentity | undefined };
const pageSize = 50, cursorLimit = 8192;
const invalid = () => new IvyError('native_inventory_invalid', 'Native task history did not return a bounded page with an explicit continuation.');

/** One native page, without collecting or retaining the rest of the history.
 * A continuation belongs to its original account, native generation and filter. */
export async function nativeThreadPage(client: RpcClient, scope: Owner,
  query: { archived: boolean; cursor?: string | undefined }, signal?: AbortSignal): Promise<{ items: NativeThreadListItem[]; nextCursor: string | null }> {
  const { archived, cursor: continuationText } = query;
  return nativeAccountRead(client, scope, async (owner, status) => {
    validateAgent('Status', status);
    const identity = canonical([owner.serviceNodeId, owner.backend ?? null, status.hostId, status.epoch,
      status.nativeVersion, status.nativeExecutableHash, status.catalogHash, archived]);
    let cursor: string | undefined;
    if (continuationText !== undefined) {
      let continuation;
      try { continuation = JSON.parse(continuationText.length <= 65536 ? continuationText : '') as unknown; } catch { /* Checked below. */ }
      const value = record(continuation);
      if (Object.keys(value).length !== 3 || value.version !== 1 || value.owner !== identity ||
        typeof value.cursor !== 'string' || !value.cursor.length || value.cursor.length > cursorLimit)
        throw new IvyError('invalid_cursor', 'Task history changed. Restart from the first page.');
      cursor = value.cursor;
    }
    const method = nativeMethod(owner.backend, 'thread/list');
    const binding = await optionalTool(client, owner.serviceNodeId, method);
    const root = binding?.definition.inputSchema, fields = record(schemaNode(root, root).properties);
    const sourceKinds = enumStrings(record(schemaNode(fields.sourceKinds, root)).items, root);
    const params = { archived, limit: pageSize, sourceKinds, modelProviders: [],
      sortKey: 'recency_at', sortDirection: 'desc', useStateDbOnly: true, ...(cursor ? { cursor } : {}) };
    if (!binding || binding.definition.nativeMethod !== 'thread/list' || binding.definition.nativeSchemaIdentity !== status.catalogHash ||
      !sourceKinds.length || !supportsFields(binding, Object.keys(params)))
      throw new IvyError('native_method_unsupported', 'This account does not advertise paginated task history.');
    const page = record(await nativeRead(client, owner.serviceNodeId, method, params, signal));
    if (!Array.isArray(page.data) || page.data.length > pageSize ||
      !(page.nextCursor === null || typeof page.nextCursor === 'string' && page.nextCursor.length > 0 &&
        page.nextCursor.length <= cursorLimit && page.nextCursor !== cursor)) throw invalid();
    const ids = new Set<string>(), items: NativeThreadListItem[] = [];
    for (const value of page.data) {
      const thread = record(value);
      if (typeof thread.id !== 'string' || !thread.id.length || thread.id.length > 256 || ids.has(thread.id)) throw invalid();
      ids.add(thread.id);
      // Preserve the native page boundary when hiding ephemeral service contexts
      // and guardian reviews; filtering must not initiate another native read.
      if (thread.ephemeral === true || record(record(thread.source).subAgent).other === 'guardian') continue;
      items.push({ id: thread.id, name: text(thread.name).slice(0, 512), preview: text(thread.preview).slice(0, 2048), cwd: text(thread.cwd).slice(0, 2048) });
    }
    const nextCursor = page.nextCursor === null ? null : JSON.stringify({ version: 1, owner: identity, cursor: page.nextCursor });
    if (nextCursor && nextCursor.length > 65536) throw invalid();
    return { items, nextCursor };
  }, signal);
}
