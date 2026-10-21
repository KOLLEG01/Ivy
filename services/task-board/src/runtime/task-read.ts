import type { TaskBoard } from '../../../../packages/sdk/src/node.js';
import type { CommentTree } from '../../../../packages/sdk/src/task-comments.js';
import type { Document, TaskBoardStore } from './store.js';
import { commentSummary } from './comment-store.js';
import { wakeCondition } from './wake.js';
import { taskCard } from './task-card.js';
import { upgradeTaskRecord } from '../../../../packages/sdk/src/client.js';

/** Read only the header and requested comment ranges. Sequence cursors survive appends. */
export async function readTask(store: TaskBoardStore, header: Document<'task-board/task'>, query: TaskBoard.TaskQuery,
  condition: (task: TaskBoard.Task, wake?: TaskBoard.ExecutionCondition) => TaskBoard.ExecutionCondition): Promise<TaskBoard.ReadResult> {
  let task = upgradeTaskRecord(header.value);
  const tree = (task as TaskBoard.Task & { commentStore?: CommentTree }).commentStore;
  const indexed = tree?.kind === 'tree' && store.commentSummary(task, tree, header.pin.objectId);
  // Retained versions are readable while the bounded storage backfill finishes.
  if (!indexed) task = (await store.read('task-board/task', header.pin, true, true)).value;
  const summary = indexed || commentSummary(task, header.pin.objectId);
  const total = indexed ? tree.length : task.comments.length;
  const wake = task.claim || task.publication ? undefined :
    wakeCondition(task, summary.questions.filter(question => question.open));
  const executionCondition = condition(task, wake);
  const view: TaskBoard.TaskView = { object: header.pin, task, executionCondition, archived: header.metadata.effectivelyArchived };
  if (query.view === 'card') return taskCard(view, summary, total);

  const limit = query.comments?.limit ?? 50;
  const before = Math.min(query.comments?.before ?? total + 1, total + 1);
  const ascending = query.comments?.offset !== undefined;
  const candidates = query.comments?.questionsOnly
    ? summary.questions.filter(question => question.open && question.sequence < before).map(question => question.sequence)
    : null;
  const end = before - 1;
  const offset = ascending ? Math.min(query.comments!.offset!, candidates?.length ?? total) :
    Math.max(0, (candidates?.length ?? end) - limit);
  const sequences = candidates ? candidates.slice(offset, offset + limit) : null;
  const range = (start: number, end: number) => indexed ? store.commentRange(tree, header.pin.objectId, start, end) :
    Promise.resolve(task.comments.slice(start, end).map(comment => ({ comment,
      receipt: task.commentDeliveries.find(receipt => receipt.commentId === comment.commentId) ?? null })));
  const selected = sequences ? [] : await range(offset, Math.min(offset + limit, ascending ? total : end));
  if (sequences) for (let index = 0; index < sequences.length; index += 8)
    selected.push(...(await Promise.all(sequences.slice(index, index + 8).map(sequence => range(sequence - 1, sequence)))).flat());
  const userQuestions: TaskBoard.Comment[] = [];
  const pending = summary.questions.filter(question => question.open && question.recipient?.kind !== 'task').slice(-100);
  for (let index = 0; index < pending.length; index += 8)
    userQuestions.push(...(await Promise.all(pending.slice(index, index + 8).map(async question =>
      selected.find(entry => entry.comment.commentId === question.commentId)?.comment ??
      (await range(question.sequence - 1, question.sequence))[0]!.comment))));
  if (!ascending) selected.reverse();
  const { commentStore: _store, ...content } = task as TaskBoard.Task & { commentStore?: CommentTree };
  return { ...view, task: { ...content, comments: selected.map(entry => entry.comment),
    commentDeliveries: selected.flatMap(entry => entry.receipt ? [entry.receipt] : []) },
    openQuestions: userQuestions, commentPage: { total: candidates ? summary.questions.filter(question => question.open).length : total,
      offset, nextOffset: ascending && offset + selected.length < (candidates?.length ?? total) ? offset + selected.length : null,
      nextBefore: offset > 0 && selected.length ? Math.min(...selected.map(entry => entry.comment.sequence)) : null,
      order: ascending ? 'asc' : 'desc' } };
}
