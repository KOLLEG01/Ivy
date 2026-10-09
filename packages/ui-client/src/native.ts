import { callBound, canonical, discover, IvyError, newOperationId } from '../../sdk/src/client.js';
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
// Panels share discovery, including absent optional capabilities. Invocations still carry the
// exact definition hash; an unexecuted read may rediscover a changed definition once.
const bindingMs = 60_000;
// All panels share one short readiness observation for their selected owner. An
// unavailable owner must not receive a separate failing call from every loader.
const readinessMs = 5_000;
type ReadReadiness = { until: number; check?: Promise<void>; error?: IvyError };
const readReadiness = new WeakMap<RpcClient, Map<string, ReadReadiness>>();
async function readyForRead(client: RpcClient, node: string): Promise<void> {
  let cache = readReadiness.get(client);
  if (!cache) readReadiness.set(client, cache = new Map());
  let entry = cache.get(node);
  if (!entry || Date.now() >= entry.until) {
    const check = client.request('serviceNodes.get', { serviceNodeId: node }, { timeoutMs: 5000 }).then(owner => {
      if (!owner.connected) throw new IvyError('service_unavailable', 'Provider connection is closed.');
      if (!owner.synced || !owner.ready || !owner.desiredEnabled) throw new IvyError('service_not_ready', 'Provider is not ready.');
    });
    cache.set(node, entry = { until: Date.now() + readinessMs, check });
  }
  if (entry.error) throw entry.error;
  await entry.check;
}
const readBindings = new WeakMap<RpcClient, Map<string, { at: number; binding: Promise<BoundTool | undefined> }>>();
const nativeCatalogs = new WeakMap<RpcClient, Map<string, string>>();
function observeCatalog(client: RpcClient, node: string, value: Wire.Json): void {
  const status = record(value), fields = ['epoch', 'nativeVersion', 'nativeExecutableHash', 'catalogHash'];
  if (status.serviceNodeId !== node || fields.some(field => typeof status[field] !== 'string' || !status[field])) return;
  let catalogs = nativeCatalogs.get(client);
  if (!catalogs) nativeCatalogs.set(client, catalogs = new Map());
  const identity = canonical(fields.map(field => status[field]!)), previous = catalogs.get(node);
  if (previous && previous !== identity) {
    const bindings = readBindings.get(client);
    for (const key of bindings?.keys() ?? []) if (key.startsWith(node + '\u0000')) bindings!.delete(key);
  }
  catalogs.set(node, identity);
}
type BindingRequest = { method: string; resolve: (binding: BoundTool | undefined) => void; reject: (error: unknown) => void };
const bindingRequests = new WeakMap<RpcClient, Map<string, BindingRequest[]>>();
async function describeBindings(client: RpcClient, node: string, methods: string[]): Promise<Map<string, BoundTool>> {
  if (methods.length === 1) {
    try {
      const binding = await discover(client, methods[0]!, { serviceNodeId: node });
      return new Map([[methods[0]!, binding]]);
    } catch (error) {
      if (error instanceof IvyError && ['not_found', 'namespace_not_found'].includes(error.code)) return new Map();
      throw error;
    }
  }
  try {
    const result = await client.request('discovery.describe', { serviceName: 'agent-manager', serviceNodeId: node, tools: methods });
    if (result.provider?.serviceNodeId !== node || result.items.length !== methods.length ||
      new Set(result.items.map(binding => binding.qualifiedName)).size !== methods.length ||
      result.items.some(binding => !methods.includes(binding.qualifiedName)))
      throw new IvyError('provider_contract_error', 'Capability discovery returned another owner or tool selection.');
    return new Map(result.items.map(binding => [binding.qualifiedName, { ...binding, serviceNodeId: node }]));
  } catch (error) {
    // An absent optional method or the schema byte budget must not discard other definitions.
    if (!(error instanceof IvyError) || !['not_found', 'namespace_not_found', 'result_too_large'].includes(error.code)) throw error;
    const middle = Math.ceil(methods.length / 2);
    const halves = await Promise.all([describeBindings(client, node, methods.slice(0, middle)), describeBindings(client, node, methods.slice(middle))]);
    return new Map(halves.flatMap(half => [...half]));
  }
}
function queueBinding(client: RpcClient, node: string, method: string): Promise<BoundTool | undefined> {
  let providers = bindingRequests.get(client);
  if (!providers) bindingRequests.set(client, providers = new Map());
  let requests = providers.get(node);
  if (!requests) {
    providers.set(node, requests = []);
    const selected = requests;
    queueMicrotask(() => {
      providers!.delete(node);
      // The existing detail endpoint accepts at most five exact definitions per request.
      for (let offset = 0; offset < selected.length; offset += 5) {
        const group = selected.slice(offset, offset + 5);
        void describeBindings(client, node, group.map(request => request.method)).then(
          bindings => { for (const request of group) request.resolve(bindings.get(request.method)); },
          error => { for (const request of group) request.reject(error); },
        );
      }
    });
  }
  const selected = requests;
  return new Promise((resolve, reject) => selected.push({ method, resolve, reject }));
}
function readBinding(client: RpcClient, node: string, method: string): Promise<BoundTool | undefined> {
  let cache = readBindings.get(client);
  if (!cache) readBindings.set(client, cache = new Map());
  const key = node + '\u0000' + method, cached = cache.get(key);
  if (cached && Date.now() - cached.at < bindingMs) return cached.binding;
  const binding = queueBinding(client, node, method);
  cache.set(key, { at: Date.now(), binding });
  binding.catch(() => { if (cache.get(key)?.binding === binding) cache.delete(key); });
  return binding;
}
async function executeRead(client: RpcClient, node: string, method: string, args: Wire.Json, signal: AbortSignal): Promise<Wire.Json> {
  for (let attempt = 0; ; attempt++) {
    signal?.throwIfAborted();
    await readyForRead(client, node);
    signal?.throwIfAborted();
    const binding = await readBinding(client, node, method);
    if (!binding) throw new IvyError('not_found', 'The exact tool is not in the selected provider catalog.');
    try {
      signal?.throwIfAborted();
      const value = await callBound(client, binding, args, method.startsWith('codex.') ? await newOperationId(client) : undefined, signal ? { signal } : {});
      if (method === 'agent.status') observeCatalog(client, node, value);
      return value;
    } catch (error) {
      if (!signal?.aborted && error instanceof IvyError && error.code === 'tool_definition_changed')
        readBindings.get(client)?.delete(node + '\u0000' + method);
      if (!signal?.aborted && error instanceof IvyError && ['service_unavailable', 'service_not_ready', 'native_capacity'].includes(error.code))
        readReadiness.get(client)?.set(node, { until: Date.now() + readinessMs, error });
      if (attempt || !(error instanceof IvyError) || error.code !== 'tool_definition_changed') throw error;
    }
  }
}
type SharedRead = { work: Promise<Wire.Json>; controller: AbortController; users: number };
const pendingReads = new WeakMap<RpcClient, Map<string, SharedRead>>();
/** Coalesce simultaneous identical observations. A cancelled panel cannot cancel other readers. */
export async function nativeRead(client: RpcClient, node: string, method: string, args: Wire.Json, signal?: AbortSignal): Promise<Wire.Json> {
  signal?.throwIfAborted();
  let reads = pendingReads.get(client);
  if (!reads) pendingReads.set(client, reads = new Map());
  const key = canonical([node, method, args]);
  let entry = reads.get(key);
  if (!entry || entry.controller.signal.aborted) {
    const controller = new AbortController();
    const work = executeRead(client, node, method, args, controller.signal);
    const current = entry = { work, controller, users: 0 };
    reads.set(key, current);
    const clear = () => { if (reads.get(key) === current) reads.delete(key); };
    void work.then(clear, clear);
  }
  const current = entry;
  current.users++;
  return new Promise<Wire.Json>((resolve, reject) => {
    let finished = false;
    const finish = (error: unknown, value?: Wire.Json) => {
      if (finished) return;
      finished = true;
      signal?.removeEventListener('abort', abort);
      if (--current.users === 0) current.controller.abort();
      if (error !== undefined) reject(error); else resolve(value!);
    };
    const abort = () => finish(signal!.reason);
    signal?.addEventListener('abort', abort, { once: true });
    void current.work.then(value => finish(undefined, value), error => finish(error));
    if (signal?.aborted) abort();
  });
}
export async function optionalTool(client: RpcClient, node: string, method: string) {
  return readBinding(client, node, method);
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
