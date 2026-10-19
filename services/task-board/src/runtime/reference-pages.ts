import { hashJson, requireThat } from '../../../../packages/sdk/src/node.js';
import type { TaskBoard, Wire } from '../../../../packages/sdk/src/node.js';
import type { RpcClient } from '../../../../packages/sdk/src/client.js';

export const referencePageKey = 'task-board/reference-page';
const pageSize = 256;

/** Exact page revisions keep overflowing evidence reachable from every retained ticket snapshot. */
export async function taskReferences(client: RpcClient, taskId: string, named: Wire.RevisionReferences,
  attachments: TaskBoard.ObjectPin[], mutation: (scope: string) => Promise<string>): Promise<Wire.RevisionReferences> {
  const references = { ...named }, seen = new Set(Object.values(named).map(hashJson));
  const pins = attachments.filter(pin => {
    const identity = hashJson(pin);
    if (seen.has(identity)) return false;
    seen.add(identity); return true;
  });
  if (Object.keys(references).length + pins.length <= pageSize) {
    for (const [index, pin] of pins.entries()) references['attachment-' + index] = pin;
    return references;
  }
  requireThat(Object.keys(references).length + Math.ceil(pins.length / pageSize) <= pageSize,
    'limit_exceeded', 'Task evidence exceeds its reference page capacity.');
  for (let offset = 0; offset < pins.length; offset += pageSize) {
    const index = offset / pageSize, name = 'Task references ' + (index + 1);
    const page = await client.request('objects.query', { contractKey: referencePageKey, includeArchived: true,
      where: { op: 'and', args: [{ field: 'object.parentId', op: 'eq', value: taskId },
        { field: 'object.name', op: 'eq', value: name }] }, limit: 2 });
    requireThat(page.items.length <= 1 && !page.nextCursor, 'task_board_identity_conflict', 'A task reference page must have one identity.');
    const previous = page.items[0] ? await client.request('objects.read', { objectId: page.items[0].objectId }) : null;
    requireThat(!previous || previous.object.ownerObjectId === taskId && previous.object.parentId === taskId,
      'task_board_scope_mismatch', 'The reference page belongs to another ticket.');
    const selected = pins.slice(offset, offset + pageSize);
    const content: Wire.Content = { encoding: 'json', value: { schemaVersion: 1, taskId, pins: selected } };
    const exact = Object.fromEntries(selected.map((pin, i) => ['pin-' + i, pin]));
    if (previous && hashJson(previous.content) === hashJson(content) && hashJson(previous.revision.references) === hashJson(exact)) {
      references['reference-page-' + index] = { objectId: previous.object.id, revision: previous.revision.revision };
      continue;
    }
    const saved = await client.request('objects.write', { contractVersion: '1.0.0', content, references: exact,
      mutationId: await mutation('reference-page:' + hashJson({ taskId, index, content, previous: previous?.revision.revision ?? 0 })),
      ...(previous ? { objectId: previous.object.id, expectedRevision: previous.revision.revision,
        expectedArchived: previous.object.effectivelyArchived } :
        { create: { contractKey: referencePageKey, parentId: taskId, ownerObjectId: taskId, name } }) });
    references['reference-page-' + index] = { objectId: saved.object.id, revision: saved.revision.revision };
  }
  return references;
}
