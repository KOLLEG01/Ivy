import type { Agent, Operation, RpcClient, Wire } from '../../sdk/src/client.js';
import { validateAgent } from '../../contracts/src/agent-validation.js';
import { backendRead, nativeAccountRead, sameBackend } from './native-backends.js';
import type { NativeBackendScope } from './native-backends.js';
import { canonical } from '../../sdk/src/client.js';

type InventoryNode = Pick<Operation.ServiceNode, 'serviceNodeId' | 'hostId' | 'connected' | 'nativeVersion'>;
export type NativeInventoryOrigin = { scope: NativeBackendScope; originalServiceNodeId: string };
const sameConnection = (a: NativeBackendScope, b: NativeBackendScope) => {
  const left = a.status, right = b.status;
  return a.serviceNodeId === b.serviceNodeId && a.hostId === b.hostId && sameBackend(a.backend, b.backend) &&
    left.state === 'ready' && right.state === 'ready' && !!left.epoch &&
    (['epoch', 'nativeVersion', 'nativeExecutableHash', 'catalogHash'] as const).every(key => left[key] === right[key]);
};

/** Read provenance only when a disconnected former single-provider owner could be duplicated. */
export async function nativeInventoryOrigins(client: RpcClient, nodes: InventoryNode[], scopes: NativeBackendScope[], signal?: AbortSignal): Promise<NativeInventoryOrigin[]> {
  signal?.throwIfAborted();
  return (await Promise.all(scopes.filter(scope => scope.backend && scope.status.state === 'ready' && nodes.some(node =>
    !node.connected && node.hostId === scope.hostId && node.serviceNodeId !== scope.serviceNodeId && node.nativeVersion === scope.status.nativeVersion))
    .map(async scope => {
      try {
        return await nativeAccountRead(client, scope, async (owner, before) => {
          const identity = await backendRead(client, owner, 'agent.storageIdentity', {}, signal);
          validateAgent('StorageIdentity', identity);
          const value = identity as Agent.StorageIdentity;
          if (value.provider !== scope.backend!.provider || !sameConnection(scope, { ...scope, status: before as Agent.Status }) ||
              !nodes.some(node => node.serviceNodeId === value.originalServiceNodeId && node.hostId === scope.hostId &&
                !node.connected && node.serviceNodeId !== scope.serviceNodeId && node.nativeVersion === scope.status.nativeVersion)) return [];
          return [{ scope, originalServiceNodeId: value.originalServiceNodeId }];
        }, signal);
      } catch {
        signal?.throwIfAborted();
        return [];
      }
    }))).flat();
}

/** Hide only a proven duplicate already present in this view; keep every unmatched original. */
export function currentNativeInventory<T>(items: T[], reference: (item: T) => Wire.ResourceRef, origins: NativeInventoryOrigin[], scopes: NativeBackendScope[], nodes: InventoryNode[]): T[] {
  const current = origins.filter(origin => origin.scope.backend && scopes.some(scope => sameConnection(origin.scope, scope)) && nodes.some(node =>
    node.serviceNodeId === origin.originalServiceNodeId && node.hostId === origin.scope.hostId && !node.connected && node.nativeVersion === origin.scope.status.nativeVersion));
  const unique = current.filter(origin => current.filter(value => value.originalServiceNodeId === origin.originalServiceNodeId).length === 1);
  const key = (ref: Wire.ResourceRef) => canonical([ref.serviceNodeId, ref.namespace, ref.kind, ref.nativeId]);
  const present = new Set(items.map(item => key(reference(item))));
  return items.filter(item => {
    const ref = reference(item);
    return !unique.some(origin => ref.serviceNodeId === origin.originalServiceNodeId && ref.namespace === 'codex' &&
      present.has(key({ ...ref, serviceNodeId: origin.scope.serviceNodeId, namespace: origin.scope.backend!.backendId })));
  });
}
