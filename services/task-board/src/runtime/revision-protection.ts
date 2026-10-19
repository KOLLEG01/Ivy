import { hashJson, requireThat } from '../../../../packages/sdk/src/node.js';
import type { TaskBoard, Wire, Operation } from '../../../../packages/sdk/src/node.js';
import type { RpcClient } from '../../../../packages/sdk/src/client.js';

const key = 'task-board/revision-protection';
type Source = { run: TaskBoard.ObjectPin; source: TaskBoard.ObjectPin };

/** Bridge legacy source pins without changing accepted workflow snapshots or Run identities. */
export async function protectRunSources(client: RpcClient, rootObjectId: string | null,
  mutation: (scope: string) => Promise<string>): Promise<void> {
  const sources: Source[] = [];
  let cursor: string | null = null;
  do {
    const page: Operation.ObjectsQueryResult = await client.request('objects.query', { contractKey: 'task-board/run', includeArchived: true,
      where: { op: 'and', args: [{ field: 'object.ownerObjectId', op: 'isNull' },
        { op: 'not', arg: { field: 'data:/phase', op: 'in', value: ['completed', 'failed', 'cancelled'] } }] },
      limit: 100, ...(cursor ? { cursor } : {}) });
    for (const item of page.items) {
      const read = await client.request('objects.read', { objectId: item.objectId });
      requireThat(read.object.contractKey === 'task-board/run' && read.content.encoding === 'json',
        'task_board_storage_invalid', 'Run source protection requires a primary JSON Run.');
      if (read.object.ownerObjectId !== null) continue;
      const run = read.content.value as unknown as TaskBoard.Run;
      if (['completed', 'failed', 'cancelled'].includes(run.phase)) continue;
      const source = { objectId: run.taskId, revision: run.originalRequest.expectedRevision };
      const existing = read.revision.references['source-task'];
      if (existing?.objectId === source.objectId && existing.revision === source.revision) continue;
      sources.push({ run: { objectId: read.object.id, revision: read.revision.revision }, source });
    }
    cursor = page.nextCursor;
  } while (cursor);
  sources.sort((a, b) => a.run.objectId.localeCompare(b.run.objectId));
  const ledgers = [];
  do {
    const page = await client.request('objects.query', { contractKey: key, includeArchived: true, limit: 100, select: ['object.name'],
      where: rootObjectId === null ? { field: 'object.parentId', op: 'isNull' } :
        { field: 'object.parentId', op: 'eq', value: rootObjectId }, ...(cursor ? { cursor } : {}) });
    ledgers.push(...page.items);
    cursor = page.nextCursor;
  } while (cursor);
  const needed = Math.ceil(sources.length / 100);
  for (let index = 0; index < Math.max(needed, ledgers.length); index++) {
    const name = 'Revision safety ' + (index + 1);
    const found = ledgers.find(item => item.values['object.name'] === name);
    const previous = found ? await client.request('objects.read', { objectId: found.objectId }) : null;
    const selected = sources.slice(index * 100, (index + 1) * 100);
    const value = { schemaVersion: 1, sources: selected };
    const references = Object.fromEntries(selected.map((item, pin) => ['source-' + pin, item.source]));
    if (previous && hashJson(previous.content) === hashJson({ encoding: 'json', value }) &&
        hashJson(previous.revision.references) === hashJson(references)) continue;
    if (!previous && !selected.length) continue;
    await client.request('objects.write', { ...(previous ? { objectId: previous.object.id,
      expectedRevision: previous.revision.revision, expectedArchived: previous.object.effectivelyArchived } :
      { create: { contractKey: key, parentId: rootObjectId, ownerObjectId: null, name } }),
      contractVersion: '1.0.0', mutationId: await mutation('revision-protection:' + hashJson({ name, value, previous: previous?.revision.revision ?? 0 })),
      references, content: { encoding: 'json', value: value as unknown as Wire.Json } });
  }
}
