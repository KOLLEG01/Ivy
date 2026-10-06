import { client, readDocument, rootWhere, taskContractVersions } from './runtime';
import type { Operation } from './runtime';

export interface BlockerCandidate { id: string; taskKey: string; title: string; workflowState: string }

/**
 * Tasks that may block `self`: every Task in the workspace except `self` and the Tasks that already
 * depend on it, directly or transitively, so picking a blocker can never form a cycle.
 */
export async function blockerCandidates(root: string | null, self: string | null, signal: AbortSignal): Promise<BlockerCandidate[]> {
  const tasks: Array<BlockerCandidate & { blocked: boolean }> = [], cursors = new Set<string>();
  let cursor: string | null = null;
  do {
    const page: Operation.ObjectsQueryResult = await client.request('objects.query', {
      contractKey: 'task-board/task', contractVersions: taskContractVersions, where: rootWhere(root),
      select: ['data:/taskKey', 'data:/fields/title', 'data:/workflowState', 'data:/fields/dependencies/0'],
      orderBy: [{ field: 'object.id', direction: 'asc' }], limit: 200, ...(cursor ? { cursor } : {}),
    }, { signal });
    for (const item of page.items) tasks.push({ id: item.objectId, taskKey: String(item.values['data:/taskKey']),
      title: String(item.values['data:/fields/title']), workflowState: String(item.values['data:/workflowState']),
      blocked: typeof item.values['data:/fields/dependencies/0'] === 'string' });
    cursor = page.nextCursor;
    if (cursor && cursors.has(cursor)) throw new Error('The task list could not finish. Try again.');
    if (cursor) cursors.add(cursor);
  } while (cursor);
  const excluded = new Set(self ? [self] : []);
  if (self) {
    // Only Tasks with at least one dependency can lead back to `self`; their full lists are read once.
    const edges = new Map<string, string[]>(), pending = tasks.filter(task => task.blocked && task.id !== self);
    for (let index = 0; index < pending.length; index += 8)
      await Promise.all(pending.slice(index, index + 8).map(async task =>
        edges.set(task.id, (await readDocument('task', task.id, root, signal)).value.fields.dependencies)));
    for (let changed = true; changed;) {
      changed = false;
      for (const [id, dependencies] of edges)
        if (!excluded.has(id) && dependencies.some(value => excluded.has(value))) { excluded.add(id); changed = true; }
    }
  }
  return tasks.filter(task => !excluded.has(task.id)).map(({ blocked: _blocked, ...task }) => task);
}
