import type { ChangeEntry, FieldChange } from '../../../packages/ui/src/lib/change-history.js';
import type { Operation, TaskBoard } from '../../../packages/sdk/src/client.js';

export type TaskHistoryFields = Pick<TaskBoard.Task, 'fields' | 'workflowState' | 'wakeAt' | 'dependencies' | 'lastHistory' | 'publication'>;

type Revision = Operation.RevisionMetadata;
interface Source {
  page(cursor?: string): Promise<Operation.ObjectsHistoryResult>;
  fields(revision: number): Promise<TaskHistoryFields | null>;
  actor(pin: TaskBoard.ObjectPin): Promise<string | null>;
  changes(before: TaskHistoryFields, after: TaskHistoryFields): Promise<FieldChange[]>;
}

/** One visible metadata page and at most one lookahead page; no retained body cache. */
export async function readFieldHistory(source: Source, signal: AbortSignal, cursor?: string) {
  const first = await source.page(cursor);
  const entries: ChangeEntry[] = [];
  let newer: { metadata: Revision; task: TaskHistoryFields } | undefined;
  let previousRevision: number | undefined;
  let gap = false, partial = !first.historyComplete;
  let page = first;
  for (let pass = 0; pass < 2; pass++) {
    for (const metadata of page.items) {
      signal.throwIfAborted();
      if (previousRevision !== undefined && metadata.revision !== previousRevision - 1) gap = partial = true;
      previousRevision = metadata.revision;
      const task = await source.fields(metadata.revision);
      signal.throwIfAborted();
      if (!task) { gap = partial = true; continue; }
      if (task.publication) continue;
      if (newer) {
        const changes = await source.changes(task, newer.task);
        if (changes.length) {
          const pin = newer.task.lastHistory;
          const actor = pin && pin.objectId !== task.lastHistory?.objectId ? await source.actor(pin) : null;
          entries.push({ id: String(newer.metadata.revision), at: newer.metadata.createdAt,
            actor: actor ?? 'TaskBoard', changes,
            ...(gap ? { summary: 'changed fields since the previous retained version' } : {}) });
        }
      }
      // The lookahead supplies only the predecessor of the last visible item.
      if (pass === 1) {
        signal.throwIfAborted();
        return { items: entries, nextCursor: first.nextCursor, partial };
      }
      newer = { metadata, task };
      gap = false;
    }
    if (pass === 1 || !newer || !page.nextCursor) break;
    signal.throwIfAborted();
    page = await source.page(page.nextCursor);
    partial ||= !page.historyComplete;
  }
  if (newer) {
    entries.push({ id: String(newer.metadata.revision), at: newer.metadata.createdAt, actor: 'TaskBoard', changes: [],
      summary: newer.metadata.revision === 1 ? 'created the task' : 'earlier field values are unavailable' });
    partial ||= newer.metadata.revision !== 1;
  }
  signal.throwIfAborted();
  return { items: entries, nextCursor: first.nextCursor, partial };
}
