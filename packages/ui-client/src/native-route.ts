import { canonical } from '../../sdk/src/client.js';
import type { Agent, Wire } from '../../sdk/src/client.js';
import { validateAgent } from '../../contracts/src/agent-validation.js';
import { nativeBackendKey } from './native-backends.js';

export interface NativeRouteOwner { serviceNodeId: string; namespace: string; backend?: Agent.BackendIdentity | undefined }
export const nativeOwnerKey = (owner: NativeRouteOwner): string => canonical([owner.serviceNodeId, owner.namespace, nativeBackendKey(owner.backend)]);
/** Unscoped retained drafts/actions keep their original storage address. */
export const nativeStorageOwner = (node: string, backend?: Agent.BackendIdentity): string => backend ? nativeOwnerKey({ serviceNodeId: node, namespace: backend.backendId, backend }) : node;
export const nativeResourceKey = (ref: Wire.ResourceRef, backend?: Agent.BackendIdentity): string => canonical([ref.serviceNodeId, ref.namespace, nativeBackendKey(backend), ref.kind, ref.nativeId]);

/** Route selectors describe the original owner. Missing selectors never acquire a new account. */
export function nativeRouteOwner(query: URLSearchParams): NativeRouteOwner {
  const serviceNodeId = query.get('node') ?? '', namespace = query.get('namespace') ?? 'codex';
  const values = ['backend', 'provider', 'account'].map(key => query.get(key));
  if (!values.some(value => value !== null)) {
    if (namespace !== 'codex') throw new Error('This route is missing its original provider and account.');
    return { serviceNodeId, namespace };
  }
  const backend = { backendId: values[0], provider: values[1], accountId: values[2] };
  validateAgent('BackendIdentity', backend);
  if (namespace !== backend.backendId) throw new Error('The route namespace differs from its original account.');
  return { serviceNodeId, namespace, backend: backend as Agent.BackendIdentity };
}
export function nativeRouteQuery(owner: NativeRouteOwner, values: Record<string, string> = {}): Record<string, string> {
  return { ...values, node: owner.serviceNodeId, namespace: owner.namespace, ...(owner.backend ? {
    backend: owner.backend.backendId, provider: owner.backend.provider, account: owner.backend.accountId,
  } : {}) };
}
