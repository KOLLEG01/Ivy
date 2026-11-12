import { and, client, rootWhere } from './runtime';
import type { Operation, TaskBoard } from './runtime';
import type { useBoardActions } from './board-actions';
import { nativeRead } from '../../../packages/ui-client/src/native';
import { taskFilter } from './task-filter';
import { forgetTask } from './task-read';

type Workspace = () => TaskBoard.WorkspaceInfo;
type Actions = ReturnType<typeof useBoardActions>;
const eligible = (card: TaskBoard.TaskCardView) => ['done', 'cancelled'].includes(card.task.workflowState) && !card.task.claim && !card.task.publication;

/** A bounded current card read includes archive metadata and no conversation. */
export async function readArchiveCards(workspace: TaskBoard.WorkspaceInfo, ids: string[], signal?: AbortSignal) {
  if (!ids.length || ids.length > 32) throw new Error('The archive read exceeds the board action limit.');
  const cards: TaskBoard.TaskCardView[] = [];
  for (let index = 0; index < ids.length; index += 20) {
    const part = ids.slice(index, index + 20);
    const result = await nativeRead(client, workspace.serviceNodeId, 'task-board.list', {
      view: 'cards', includeArchived: true, where: and(rootWhere(workspace.rootObjectId), { op: 'in', field: 'object.id', value: part }),
      limit: part.length,
    }, signal) as TaskBoard.CardListResult;
    if (result.nextCursor || result.items.some(card => !part.includes(card.object.objectId)) || new Set(result.items.map(card => card.object.objectId)).size !== result.items.length)
      throw new Error('The current archive cards differ from the requested tickets.');
    cards.push(...result.items);
  }
  return cards;
}

/** Single cards use a compact stat; a freshly read batch card already supplies
 * its authoritative state, revision and metadata fence for backend CAS. */
export async function archiveBoardCard(workspace: Workspace, board: Actions, card: TaskBoard.TaskCardView,
  changed: () => void, readCurrent = true) {
  const id = card.object.objectId;
  forgetTask(workspace().serviceNodeId, id);
  const admitted = await board.run(id, async (action, projection) => {
    let base = card.observation;
    const result = await action.start('Archive task', async () => {
      if (!eligible(card)) throw new Error('This task changed. Refresh the board before archiving it.');
      if (readCurrent) {
        const object = await client.request('objects.stat', { objectId: id, observe: true });
        if (object.contractKey !== 'task-board/task' || object.parentId !== workspace().rootObjectId)
          throw new Error('This ticket belongs to another workspace.');
        base = object.observation;
        projection.observe({ object: { objectId: object.id, revision: object.currentRevision }, archived: object.effectivelyArchived,
          ...(object.observation ? { observation: object.observation } : {}) });
        if (object.effectivelyArchived) { changed(); return null; }
        if (object.currentRevision !== card.object.revision)
          throw new Error('This task changed. Refresh the board before archiving it.');
      }
      return { action: 'archive', taskId: id, expectedRevision: card.object.revision, archived: true };
    }, () => base);
    if (result) { forgetTask(workspace().serviceNodeId, id); changed(); }
  }, { card, archived: true });
  return admitted;
}

/** Collect the filtered selection before any mutation changes its pages. Only
 * IDs survive collection; current state is read in ordinary card-sized batches. */
export async function archiveBoardSelection(workspace: Workspace, query: URLSearchParams, board: Actions,
  changed: () => void, blocked: (id: string) => boolean, signal: AbortSignal) {
  const selected = workspace(), filter = new URLSearchParams(query), ids: string[] = [], cursors = new Set<string>();
  let cursor: string | null = null, skipped = 0;
  do {
    signal.throwIfAborted();
    const page: Operation.ObjectsQueryResult = await client.request('objects.query', {
      contractKey: 'task-board/task',
      where: and(taskFilter(selected.rootObjectId, filter), { op: 'in', field: 'data:/workflowState', value: ['done', 'cancelled'] }),
      orderBy: [{ field: 'object.id', direction: 'asc' }], limit: 200, ...(cursor ? { cursor } : {}),
    }, { signal });
    ids.push(...page.items.map(item => item.objectId));
    cursor = page.nextCursor;
    if (cursor && cursors.has(cursor)) throw new Error('The archive selection could not finish. Refresh the board.');
    if (cursor) cursors.add(cursor);
  } while (cursor);
  for (let index = 0; index < ids.length; index += 20) {
    signal.throwIfAborted();
    const part = ids.slice(index, index + 20), cards = new Map((await readArchiveCards(selected, part, signal)).map(card => [card.object.objectId, card]));
    for (const id of part) {
      signal.throwIfAborted();
      const card = cards.get(id);
      if (card?.archived) continue;
      if (!card || !eligible(card) || blocked(id)) { skipped++; continue; }
      if (!await archiveBoardCard(workspace, board, card, changed, false)) return { skipped, stopped: true };
    }
  }
  return { skipped, stopped: false };
}
