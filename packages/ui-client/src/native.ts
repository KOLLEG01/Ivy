import { callBound, discover, IvyError, newOperationId } from '../../sdk/src/client.js';
import type { BoundTool, RpcClient, Wire } from '../../sdk/src/client.js';

export type JsonRecord = { [key: string]: Wire.Json };
export const record = (value: unknown): JsonRecord => value && typeof value === 'object' && !Array.isArray(value) ? value as JsonRecord : {};
export const list = (value: unknown): Wire.Json[] => Array.isArray(value) ? value : [];
export const text = (value: unknown, fallback = ''): string => typeof value === 'string' ? value : fallback;
export function serviceNodeLabel(nodes: readonly { serviceNodeId: string; hostId: string }[], serviceNodeId: string): string {
  const owner = nodes.find(node => node.serviceNodeId === serviceNodeId);
  if (!owner) return serviceNodeId;
  const instance = serviceNodeId.slice(owner.hostId.length + 1);
  if (instance === 'agent-manager') return owner.hostId;
  if (instance.startsWith('agent-manager-')) {
    const name = instance.slice('agent-manager-'.length);
    return owner.hostId + '.' + name.charAt(0).toUpperCase() + name.slice(1);
  }
  return nodes.some(node => node.hostId === owner.hostId && node.serviceNodeId !== serviceNodeId) ? serviceNodeId : owner.hostId;
}
export function confirmsAbsence(error: IvyError, call: Wire.ToolCall): boolean {
  const evidence = record(error.details);
  return error.code === 'not_found' && error.outcome === 'not_executed' && evidence.kind === 'agent_operation_absent' && evidence.serviceNodeId === call.serviceNodeId && evidence.operationId === call.operationId && (typeof evidence.epoch === 'string' || evidence.epoch === null);
}

// Schema inspection supports local references only. Hive/native remain authoritative validators.
export function schemaNode(value: unknown, root: unknown, depth = 0): JsonRecord {
  const node = record(value), source = record(root), ref = text(node.$ref);
  if (depth > 32) return {};
  if (ref) return ref.startsWith('#/$defs/') ? schemaNode(record(source.$defs)[ref.slice(8)], root, depth + 1) : {};
  if (Array.isArray(node.allOf) && node.allOf.length === 1) return { ...schemaNode(node.allOf[0], root, depth + 1), ...Object.fromEntries(Object.entries(node).filter(([key]) => key !== 'allOf')) };
  return node;
}
export function enumStrings(value: unknown, root: unknown, depth = 0): string[] {
  if (depth > 32) return [];
  const node = schemaNode(value, root);
  return [...new Set([...list(node.enum).filter((v): v is string => typeof v === 'string'), ...(typeof node.const === 'string' ? [node.const] : []),
    ...list(node.oneOf ?? node.anyOf).flatMap(v => enumStrings(v, root, depth + 1))])];
}
export function supportsFields(binding: BoundTool | null | undefined, supplied: string[]): boolean {
  if (!binding) return false;
  const root = schemaNode(binding.definition.inputSchema, binding.definition.inputSchema), fields = record(root.properties);
  return supplied.every(key => key in fields) && list(root.required).every(key => typeof key === 'string' && supplied.includes(key));
}
// Reads reuse a recent binding instead of repeating discovery before every call. Any failure drops
// it, and a changed definition is rediscovered once, because an unexecuted read is safe to repeat.
const bindingMs = 60_000;
const readBindings = new WeakMap<RpcClient, Map<string, { at: number; binding: Promise<BoundTool> }>>();
function readBinding(client: RpcClient, node: string, method: string): Promise<BoundTool> {
  let cache = readBindings.get(client);
  if (!cache) readBindings.set(client, cache = new Map());
  const key = node + '\u0000' + method, cached = cache.get(key);
  if (cached && Date.now() - cached.at < bindingMs) return cached.binding;
  const binding = discover(client, method, { serviceNodeId: node });
  cache.set(key, { at: Date.now(), binding });
  binding.catch(() => { if (cache.get(key)?.binding === binding) cache.delete(key); });
  return binding;
}
export async function nativeRead(client: RpcClient, node: string, method: string, args: Wire.Json, signal?: AbortSignal): Promise<Wire.Json> {
  for (let attempt = 0; ; attempt++) {
    const binding = await readBinding(client, node, method);
    try {
      return await callBound(client, binding, args, method.startsWith('codex.') ? await newOperationId(client) : undefined, signal ? { signal } : {});
    } catch (error) {
      if (!signal?.aborted) readBindings.get(client)?.delete(node + '\u0000' + method);
      if (attempt || !(error instanceof IvyError) || error.code !== 'tool_definition_changed') throw error;
    }
  }
}
export async function optionalTool(client: RpcClient, node: string, method: string) {
  try { return await discover(client, method, { serviceNodeId: node }); }
  catch (error) { if (error instanceof IvyError && ['not_found', 'namespace_not_found'].includes(error.code)) return undefined; throw error; }
}
export async function nativeModels(client: RpcClient, node: string, signal?: AbortSignal): Promise<JsonRecord> {
  const data: Wire.Json[] = [], cursors = new Set<string>(); let cursor: string | null = null;
  const timeout = AbortSignal.timeout(35000), bounded = signal ? AbortSignal.any([timeout, signal]) : timeout;
  do {
    // Hidden models include configured defaults outside the native picker; they stay selectable.
    const page = record(await nativeRead(client, node, 'codex.model/list', { limit: 100, includeHidden: true, ...(cursor ? { cursor } : {}) }, bounded));
    data.push(...list(page.data)); cursor = text(page.nextCursor) || null;
    if (data.length > 1000 || cursor && cursors.has(cursor) || cursors.size >= 20) throw new IvyError('native_models_unavailable', 'Native model discovery exceeded its bounded complete snapshot.');
    if (cursor) cursors.add(cursor);
  } while (cursor);
  return { data, nextCursor: null, observedAt: new Date().toISOString() };
}
export async function nativePermissionProfiles(client: RpcClient, node: string, cwd?: string, signal?: AbortSignal): Promise<JsonRecord> {
  const data: Wire.Json[] = [], cursors = new Set<string>(); let cursor: string | null = null;
  const timeout = AbortSignal.timeout(35000), bounded = signal ? AbortSignal.any([timeout, signal]) : timeout;
  do {
    const page = record(await nativeRead(client, node, 'codex.permissionProfile/list', { limit: 100, ...(cwd ? { cwd } : {}), ...(cursor ? { cursor } : {}) }, bounded));
    data.push(...list(page.data)); cursor = text(page.nextCursor) || null;
    if (data.length > 1000 || cursor && cursors.has(cursor) || cursors.size >= 20) throw new IvyError('native_permission_profiles_unavailable', 'Native permission profile discovery exceeded its bounded complete snapshot.');
    if (cursor) cursors.add(cursor);
  } while (cursor);
  return { data, nextCursor: null, observedAt: new Date().toISOString() };
}
export interface NativePermissionProfile { id: string; description: string; allowed: boolean }
export function permissionProfilesFrom(value: unknown): NativePermissionProfile[] {
  return list(record(value).data).flatMap(entry => { const profile = record(entry); return typeof profile.id === 'string' ? [{ id: profile.id, description: text(profile.description), allowed: profile.allowed === true }] : []; });
}
export function permissionProfileLabel(id: string): string {
  const builtIn: Record<string, string> = { ':read-only': 'Read only', ':workspace': 'Workspace', ':danger-full-access': 'Full access' };
  return builtIn[id] ?? id.replace(/^:/, '').replace(/[-_]+/g, ' ').replace(/\b\w/g, value => value.toUpperCase());
}
export interface NativeModel { id: string; model: string; name: string; description: string; efforts: string[]; defaultEffort: string; isDefault: boolean; hidden: boolean }
/** Native picker models first, then models the native picker hides. */
export function modelsFrom(value: unknown): NativeModel[] {
  const models = list(record(value).data).flatMap(entry => { const m = record(entry), efforts = list(m.supportedReasoningEfforts).map(e => text(record(e).reasoningEffort)).filter(Boolean);
    return typeof m.model === 'string' && typeof m.id === 'string' ? [{ id: m.id, model: m.model, name: text(m.displayName, m.model), description: text(m.description), efforts, defaultEffort: text(m.defaultReasoningEffort), isDefault: m.isDefault === true, hidden: m.hidden === true }] : []; });
  return [...models.filter(model => !model.hidden), ...models.filter(model => model.hidden)];
}
export function nativeText(item: unknown): string {
  const v = record(item);
  if (typeof v.text === 'string') return v.text;
  if (v.type === 'userMessage') return list(v.content).map(c => text(record(c).text)).filter(Boolean).join('\n');
  if (typeof v.aggregatedOutput === 'string') return v.aggregatedOutput;
  if (v.type === 'reasoning') return [...list(v.summary), ...list(v.content)].map(v => typeof v === 'string' ? v : text(record(v).text)).join('\n');
  if (v.type === 'fileChange') return list(v.changes).map(c => `${text(record(c).path)}\n${text(record(c).diff)}`).join('\n\n');
  return '';
}
