import { client, rootWhere, taskContractVersions } from './runtime';
import type { Operation } from './runtime';

export interface BlockerCandidate { id: string; taskKey: string; title: string; workflowState: string }


export async function blockerCandidates(root: string | null, self: string | null, signal: AbortSignal): Promise<BlockerCandidate[]> {
  const tasks: BlockerCandidate[] = [], cursors = new Set<string>();
  let cursor: string | null = null;
  do {
    const page: Operation.ObjectsQueryResult = await client.request('objects.query', {
      contractKey: 'task-board/task', contractVersions: taskContractVersions, where: rootWhere(root),
      select: ['data:/taskKey', 'data:/fields/title', 'data:/workflowState'],
      orderBy: [{ field: 'object.id', direction: 'asc' }], limit: 200, ...(cursor ? { cursor } : {}),
    }, { signal });
    for (const item of page.items) tasks.push({ id: item.objectId, taskKey: String(item.values['data:/taskKey']),
      title: String(item.values['data:/fields/title']), workflowState: String(item.values['data:/workflowState']) });
    cursor = page.nextCursor;
    if (cursor && cursors.has(cursor)) throw new Error('The task list could not finish. Try again.');
    if (cursor) cursors.add(cursor);
  } while (cursor);
  return tasks.filter(task => task.id !== self);
}
