import { canonical, IvyError } from '../../sdk/src/client.js';
import type { Agent, RpcClient, Wire } from '../../sdk/src/client.js';
import { validateAgent } from '../../contracts/src/agent-validation.js';
import { backendArguments, sameNativeBackend } from '../../contracts/src/native-backend.js';
import { modelsFrom, nativeModels, nativeRead, optionalTool, record } from './native.js';

export type NativeBackendStatus = Agent.BackendsResult['backends'][number];
export interface NativeBackendScope {
  serviceNodeId: string;
  hostId: string;
  backend?: Agent.BackendIdentity;
  status: NativeBackendStatus;
}

export const nativeBackendKey = (backend: Agent.BackendIdentity | undefined): string =>
  backend === undefined ? '' : canonical([backend.backendId, backend.provider, backend.accountId]);
export const nativeModelKey = (backend: Agent.BackendIdentity | undefined, model: string, serviceNodeId?: string): string => canonical([nativeBackendKey(backend), backend ? null : serviceNodeId ?? null, model]);
export const nativeBackendLabel = (backend: Agent.BackendIdentity): string => `${backend.provider === 'claude' ? 'Claude' : 'Codex'} · ${backend.accountId} (${backend.backendId})`;
export const sameBackend = (a: Agent.BackendIdentity | undefined, b: Agent.BackendIdentity | undefined): boolean => sameNativeBackend(a, b);
export const nativeMethod = (backend: Agent.BackendIdentity | undefined, method: string): string => `${backend?.backendId ?? 'codex'}.${method}`;

/** The selector belongs only to the Agent envelope; provider parameters stay unchanged. */
export async function backendRead(client: RpcClient, scope: { serviceNodeId: string; backend?: Agent.BackendIdentity | undefined }, method: string, args: Wire.Json, signal?: AbortSignal) {
  const selected = { serviceNodeId: scope.serviceNodeId, backend: scope.backend ? { ...scope.backend } : undefined };
  if (!method.startsWith('agent.')) return nativeAccountRead(client, selected, owner =>
    nativeRead(client, owner.serviceNodeId, nativeMethod(owner.backend, method), args, signal), signal);
  const value = await nativeRead(client, selected.serviceNodeId, method, backendArguments(args, selected.backend), signal);
  if (!sameBackend(record(value).backend as Agent.BackendIdentity | undefined, selected.backend) ||
      'serviceNodeId' in record(value) && record(value).serviceNodeId !== selected.serviceNodeId)
    throw new IvyError('native_owner_mismatch', 'The response does not belong to the selected service and account.');
  return value;
}

/** A namespace names a slot, not an immutable account. Concurrent status checks coalesce.
 * Check failed reads too: an old -32601 must not enable fallback on a replacement owner. */
export async function nativeAccountRead<T>(client: RpcClient, scope: { serviceNodeId: string; backend?: Agent.BackendIdentity | undefined },
  load: (owner: { serviceNodeId: string; backend?: Agent.BackendIdentity | undefined }, status: ReturnType<typeof record>) => Promise<T>, signal?: AbortSignal): Promise<T> {
  const selected = { serviceNodeId: scope.serviceNodeId, backend: scope.backend ? { ...scope.backend } : undefined };
  const before = record(await backendRead(client, selected, 'agent.status', {}, signal));
  if (before.state !== 'ready' || !before.epoch) throw new IvyError('native_unavailable', 'The original native account is not ready.');
  let outcome: { ok: true; value: T } | { ok: false; error: unknown };
  try { outcome = { ok: true, value: await load(selected, before) }; } catch (error) { outcome = { ok: false, error }; }
  const after = record(await backendRead(client, selected, 'agent.status', {}, signal));
  if (after.state !== 'ready' || ['epoch', 'nativeVersion', 'nativeExecutableHash', 'catalogHash'].some(key => before[key] !== after[key]))
    throw new IvyError('native_owner_changed', 'The native connection changed during this read. Refresh the original account.');
  if (!outcome.ok) throw outcome.error;
  return outcome.value;
}

/** Discover explicit owners before requesting their models, projects or permissions. */
export async function nativeBackendScopes(client: RpcClient, node: { serviceNodeId: string; hostId: string }, signal?: AbortSignal): Promise<NativeBackendScope[]> {
  signal?.throwIfAborted();
  const result = await nativeRead(client, node.serviceNodeId, 'agent.backends', {}, signal);
  validateAgent('BackendsResult', result);
  const statuses = (result as Agent.BackendsResult).backends;
  const ids = new Set<string>();
  return statuses.map(status => {
    const key = status.backend?.backendId ?? '';
    if (status.serviceNodeId !== node.serviceNodeId || status.hostId !== node.hostId || ids.has(key) || !status.backend && statuses.length !== 1)
      throw new IvyError('native_owner_mismatch', 'Backend discovery must retain its selected host, service and account.');
    ids.add(key);
    return { ...node, ...(status.backend ? { backend: status.backend } : {}), status };
  });
}

export async function nativeBackendModels(client: RpcClient, scope: NativeBackendScope, signal?: AbortSignal) {
  const before = scope.status;
  if (before.state !== 'ready' || !('epoch' in before) || !before.epoch || !sameBackend(before.backend, scope.backend) ||
    before.serviceNodeId !== scope.serviceNodeId || before.hostId !== scope.hostId)
    throw new IvyError('native_unavailable', 'The selected account is not ready.');
  const methods = ['thread/start', 'turn/start'];
  const [models, bindings] = await Promise.all([
    nativeModels(client, scope.serviceNodeId, signal, scope.backend?.backendId).then(modelsFrom),
    Promise.all(methods.map(method => optionalTool(client, scope.serviceNodeId, nativeMethod(scope.backend, method)))),
  ]);
  if (bindings.some(binding => !binding)) throw new IvyError('native_method_unsupported', 'The selected account does not support starting agent tasks.');
  const after = await backendRead(client, scope, 'agent.status', {}, signal);
  validateAgent('Status', after);
  const status = after as Agent.Status;
  if (status.serviceNodeId !== scope.serviceNodeId || status.hostId !== scope.hostId || !sameBackend(status.backend, scope.backend) || status.state !== 'ready' ||
      (['epoch', 'nativeVersion', 'nativeExecutableHash', 'catalogHash'] as const).some(key => status[key] !== before[key]) ||
      bindings.some((binding, index) => !binding || binding.serviceNodeId !== scope.serviceNodeId ||
        binding.qualifiedName !== nativeMethod(scope.backend, methods[index]!) || binding.definition.namespace !== (scope.backend?.backendId ?? 'codex') ||
        binding.definition.name !== methods[index] || binding.definition.nativeMethod !== methods[index] ||
        binding.definition.nativeSchemaIdentity !== status.catalogHash))
    throw new IvyError('native_owner_changed', 'The selected account changed during model discovery.');
  return { models, bindings: bindings.filter(binding => binding !== undefined) };
}
