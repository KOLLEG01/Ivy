import { canonical, hashJson, jsonObjectContentBytes, requireThat } from '../../../../packages/sdk/src/node.js';
import type { TaskBoard, Wire } from '../../../../packages/sdk/src/node.js';
import type { RpcClient } from '../../../../packages/sdk/src/client.js';

export const commentContractKey = 'task-board/comments';
type CommentState = Pick<TaskBoard.Comment, 'commentId' | 'nativeInput' | 'handoff' | 'delivery'>;
type CommentReference = { objectId: string; length: number; nativeInputs: CommentState[] };
type StoredTask = TaskBoard.Task & { commentStore?: CommentReference };

function taskHeader(task: TaskBoard.Task, commentObjectId: string): StoredTask {
  return { ...task, comments: [], commentStore: { objectId: commentObjectId, length: task.comments.length,
    nativeInputs: task.comments.map(comment => ({ commentId: comment.commentId, delivery: comment.delivery,
      ...(comment.nativeInput ? { nativeInput: comment.nativeInput } : {}), ...(comment.handoff ? { handoff: comment.handoff } : {}) })) } };
}

export function validateTaskContent(task: TaskBoard.Task, taskId: string): void {
  canonical(taskHeader(task, taskId), jsonObjectContentBytes - 1024);
  canonical({ schemaVersion: 1, taskId, comments: task.comments }, jsonObjectContentBytes - 1024);
}

/** The ticket pins a conversation prefix, not an obsolete revision of its append-only body. */
export class TaskComments {
  constructor(private readonly client: RpcClient, private readonly identity: (scope: string) => Promise<string>) {}

  async read(value: Wire.Json, taskId: string): Promise<TaskBoard.Task> {
    const task = value as unknown as StoredTask;
    if (!task.commentStore) return task;
    const { commentStore: reference, ...header } = task;
    const object = await this.client.request('objects.read', { objectId: reference.objectId });
    requireThat(object.object.contractKey === commentContractKey && object.object.ownerObjectId === null &&
      object.content.encoding === 'json', 'task_board_contract_mismatch', 'The ticket conversation is not a retained comment record.');
    const log = object.content.value as unknown as { taskId: string; comments: TaskBoard.Comment[] };
    requireThat(log.taskId === taskId && object.object.parentId === taskId && Array.isArray(log.comments) &&
      Number.isSafeInteger(reference.length) && reference.length >= 0 && reference.length <= log.comments.length,
    'task_board_evidence_mismatch', 'The ticket conversation prefix is unavailable.');
    const inputs = new Map(reference.nativeInputs.map(state => [state.commentId, state]));
    const comments = log.comments.slice(0, reference.length).map(comment => {
      const { nativeInput: _current, handoff: _handoff, delivery: _delivery, ...body } = comment;
      return { ...body, ...inputs.get(comment.commentId)! };
    });
    return { ...header, comments };
  }

  async write(task: TaskBoard.Task, taskId: string, scope: string): Promise<StoredTask> {
    const page = await this.client.request('objects.query', { contractKey: commentContractKey,
      where: { op: 'and', args: [{ field: 'object.parentId', op: 'eq', value: taskId },
        { field: 'object.name', op: 'eq', value: 'Comments' }] }, limit: 2, includeArchived: true });
    requireThat(page.items.length <= 1 && !page.nextCursor, 'task_board_identity_conflict', 'A ticket must have one conversation.');
    const existing = page.items[0] ? await this.client.request('objects.read', { objectId: page.items[0].objectId }) : null;
    const current = existing?.content.encoding === 'json'
      ? existing.content.value as unknown as { taskId: string; comments: TaskBoard.Comment[] } : null;
    requireThat(!existing || existing.object.ownerObjectId === null && existing.object.parentId === taskId &&
      current?.taskId === taskId && Array.isArray(current.comments),
      'task_board_scope_mismatch', 'The ticket conversation belongs to another owner.');
    const immutable = ({ nativeInput: _input, handoff: _handoff, delivery: _delivery, ...body }: TaskBoard.Comment) => body;
    for (let index = 0; index < Math.min(task.comments.length, current?.comments.length ?? 0); index++)
      requireThat(hashJson(immutable(task.comments[index]!)) === hashJson(immutable(current!.comments[index]!)),
        'task_board_comment_mismatch', 'Published comment content is immutable; only conversation metadata can change.');
    // A lost header reply must not truncate a conversation already appended by this publication.
    const comments = [...task.comments, ...(current?.comments.slice(task.comments.length) ?? [])];
    const value = { schemaVersion: 1, taskId, comments };
    let objectId = existing?.object.id;
    if (!existing || hashJson(existing.content.value) !== hashJson(value)) {
      const mutationId = await this.identity(hashJson({ scope, comments: value, previous: existing?.revision.revision ?? 0 }));
      const saved = await this.client.request('objects.write', { mutationId, contractVersion: '1.1.0',
        content: { encoding: 'json', value: value as unknown as Wire.Json }, references: {},
        ...(existing ? { objectId: existing.object.id, expectedRevision: existing.revision.revision } :
          { create: { name: 'Comments', parentId: taskId, ownerObjectId: null, contractKey: commentContractKey } }) });
      objectId = saved.object.id;
    }
    return taskHeader(task, objectId!);
  }
}
