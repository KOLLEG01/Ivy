import { IvyError } from '../../sdk/src/client.js';
import type { Operation, RpcClient } from '../../sdk/src/client.js';

export interface NativeHostNode { serviceNodeId: string; hostId: string; connected: boolean; ready: boolean }
export interface NativeHost { hostId: string; serviceNodeId: string; connected: boolean; ready: boolean; nodes: NativeHostNode[] }

export async function nativeServiceNodes(client: RpcClient, signal?: AbortSignal, hostId?: string): Promise<Operation.ServiceNode[]> {
  const nodes: Operation.ServiceNode[] = [], cursors = new Set<string>();
  let cursor: string | undefined;
  do {
    const page = await client.request('serviceNodes.list', { serviceName: 'agent-manager', ...(hostId ? { hostId } : {}), limit: 200,
      ...(cursor ? { cursor } : {}) }, signal ? { signal } : undefined);
    nodes.push(...page.items); cursor = page.nextCursor ?? undefined;
    if (nodes.length > 1000 || cursor && (cursors.has(cursor) || cursors.size >= 19))
      throw new IvyError('native_hosts_unavailable', 'Host discovery exceeded its bounded complete snapshot.');
    if (cursor) cursors.add(cursor);
  } while (cursor);
  return nodes;
}

/** Host presentation is shared; every operation still selects its exact service and account. */
export function nativeHosts(nodes: NativeHostNode[]): NativeHost[] {
  const hosts = new Map<string, NativeHostNode[]>();
  for (const node of nodes) {
    const own = hosts.get(node.hostId) ?? [];
    if (!own.some(value => value.serviceNodeId === node.serviceNodeId)) own.push(node);
    hosts.set(node.hostId, own);
  }
  return [...hosts].map(([hostId, own]) => {
    const selected = own.find(node => node.connected && node.ready) ?? own.find(node => node.connected) ?? own[0]!;
    return { hostId, serviceNodeId: selected.serviceNodeId, ready: own.some(node => node.ready && node.connected), connected: own.some(node => node.connected), nodes: own };
  });
}
