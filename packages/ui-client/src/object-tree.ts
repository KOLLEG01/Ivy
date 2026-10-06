import type { HiveClient, Operation } from '../../sdk/src/client.js';
import type { HierarchyLoader, HierarchyNode } from '@ivy/ui';

export function objectTreeLoader(client: HiveClient, href: (id: string) => string, contractKey?: string): HierarchyLoader {
  return async (parentId, cursor, signal) => {
    const page = { limit: 50, ...(cursor ? { cursor } : {}) };
    if (contractKey) {
      const result = await client.request('objects.query', { ...page, contractKey, where: parentId ? { op: 'eq', field: 'object.parentId', value: parentId } : { op: 'isNull', field: 'object.parentId' }, select: ['object.name', 'object.icon', 'object.hasContractChildren'], orderBy: [{ field: 'object.position', direction: 'asc' }] }, { signal });
      return { items: result.items.map(item => ({ id: item.objectId, label: String(item.values['object.name']), icon: item.values['object.icon'] === null ? null : String(item.values['object.icon']), hasChildren: item.values['object.hasContractChildren'] === true, href: href(item.objectId) })), nextCursor: result.nextCursor };
    }
    const result = await client.request('objects.list', { ...page, parentId }, { signal });
    return { items: result.items.map(item => ({ id: item.id, label: item.name, icon: item.icon, href: href(item.id) })), nextCursor: result.nextCursor };
  };
}

export async function objectAncestors(client: HiveClient, id: string, href: (id: string) => string, signal: AbortSignal): Promise<HierarchyNode[]> {
  const result: HierarchyNode[] = [], visited = new Set<string>(); let current: string | null = id;
  while (current) {
    if (visited.has(current) || visited.size >= 128) throw new Error('This hierarchy could not be followed. Open the object from the root.');
    visited.add(current);
    const item: Operation.ObjectMetadata = await client.request('objects.stat', { objectId: current }, { signal });
    result.unshift({ id: item.id, label: item.name, href: href(item.id) }); current = item.parentId;
  }
  return result;
}
