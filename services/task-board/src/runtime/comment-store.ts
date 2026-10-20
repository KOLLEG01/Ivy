import { canonical, hashJson, jsonObjectContentBytes, requireThat } from '../../../../packages/sdk/src/node.js';
import type { TaskBoard, Wire } from '../../../../packages/sdk/src/node.js';
import type { RpcClient } from '../../../../packages/sdk/src/client.js';
import { CommentReader, commentNodeKey } from '../../../../packages/sdk/src/task-comments.js';
import type { CommentNode, CommentIndex, CommentTree, CommentSummary } from '../../../../packages/sdk/src/task-comments.js';
import { crossAgentComment, openQuestions } from './conversation.js';

export const commentContractKey = 'task-board/comments';
type CommentState = Pick<TaskBoard.Comment, 'commentId' | 'nativeInput' | 'handoff' | 'delivery'>;
type CommentReference = { objectId: string; length: number; nativeInputs: CommentState[] };
type StoredTask = TaskBoard.Task & { commentStore?: CommentReference };
type TreeTask = TaskBoard.Task & { commentStore: CommentTree };
const referencedQuestions = (task: TaskBoard.Task) => task.nextSteps.flatMap(step => step.requires.flatMap(condition =>
  condition.kind === 'question' || condition.kind === 'user' ? [condition.questionId] : []));

export function commentSummary(task: TaskBoard.Task, taskId: string): CommentSummary {
  const open = new Set(openQuestions(task).map(question => question.commentId));
  const referenced = new Set(referencedQuestions(task));
  return {
    directCount: task.comments.filter(comment => !crossAgentComment(comment, taskId)).length,
    questions: task.comments.filter(comment => comment.purpose === 'question' && !comment.sourceTaskId &&
      (open.has(comment.commentId) || referenced.has(comment.commentId))).map(comment => ({
        commentId: comment.commentId, sequence: comment.sequence, open: open.has(comment.commentId),
        ...(comment.recipient ? { recipient: comment.recipient } : {}),
      })),
  };
}

function taskHeader(task: TaskBoard.Task, commentObjectId: string): StoredTask {
  return { ...task, comments: [], commentStore: { objectId: commentObjectId, length: task.comments.length,
    nativeInputs: task.comments.map(comment => ({ commentId: comment.commentId, delivery: comment.delivery,
      ...(comment.nativeInput ? { nativeInput: comment.nativeInput } : {}), ...(comment.handoff ? { handoff: comment.handoff } : {}) })) } };
}

export function validateTaskContent(task: TaskBoard.Task, taskId: string): void {
  canonical({ ...task, comments: [], commentDeliveries: [], commentStore: { kind: 'tree', root: null, length: 0 } }, jsonObjectContentBytes - 1024);
  const receipts = new Map(task.commentDeliveries.map(receipt => [receipt.commentId, receipt]));
  for (const comment of task.comments) canonical({ schemaVersion: 1, taskId, kind: 'comment', entry: {
    comment, receipt: receipts.get(comment.commentId) ?? null,
  } }, jsonObjectContentBytes - 1024);
}

/** The ticket pins a conversation prefix, not an obsolete revision of its append-only body. */
export class TaskComments {
  private readonly reader: CommentReader;
  private readonly pins = new Map<string, TaskBoard.ObjectPin>();
  // Active runs may retain the previous header contract until their next publication.
  // Cache only small projections, keyed by immutable history and the referenced gates.
  private readonly summaries = new Map<string, CommentSummary>();
  private summaryKey(task: TaskBoard.Task, tree: CommentTree, taskId: string) {
    return hashJson({ taskId, root: tree.root, questions: [...new Set(referencedQuestions(task))].sort() });
  }
  summary(task: TaskBoard.Task, tree: CommentTree, taskId: string): CommentSummary | undefined {
    return tree.summary ?? this.summaries.get(this.summaryKey(task, tree, taskId));
  }
  constructor(private readonly client: RpcClient, private readonly identity: (scope: string) => Promise<string>) {
    this.reader = new CommentReader(client);
    const read = this.reader.node.bind(this.reader);
    this.reader.node = async (pin, taskId, signal) => {
      const node = await read(pin, taskId, signal);
      this.remember(hashJson(node), pin);
      return node;
    };
  }
  private remember(hash: string, pin: TaskBoard.ObjectPin): void {
    if (this.pins.size >= 16384) this.pins.delete(this.pins.keys().next().value!);
    this.pins.set(hash, pin);
  }

  range(tree: CommentTree, taskId: string, start: number, end: number) {
    return this.reader.range(tree, taskId, start, end);
  }

  async read(value: Wire.Json, taskId: string): Promise<TaskBoard.Task> {
    const task = value as unknown as StoredTask;
    if (!task.commentStore) return task;
    if ('kind' in task.commentStore) {
      const { commentStore, ...header } = task as unknown as TreeTask;
      const entries = await this.reader.range(commentStore, taskId);
      const full = { ...header, comments: entries.map(entry => entry.comment),
        commentDeliveries: entries.flatMap(entry => entry.receipt ? [entry.receipt] : []) };
      if (!commentStore.summary) {
        const summary = commentSummary(full, taskId);
        if (Buffer.byteLength(JSON.stringify(summary)) <= 64 * 1024) {
          if (this.summaries.size >= 128) this.summaries.delete(this.summaries.keys().next().value!);
          this.summaries.set(this.summaryKey(full, commentStore, taskId), summary);
        }
      }
      return full;
    }
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

  async write(task: TaskBoard.Task, taskId: string, scope: string, contractVersion: string = '2.3.0'): Promise<StoredTask | TreeTask> {
    if (!['2.2.0', '2.3.0'].includes(contractVersion)) return this.writeRetained(task, taskId, scope, contractVersion);
    const current = await this.client.request('objects.read', { objectId: taskId });
    requireThat(current.content.encoding === 'json' && current.object.contractKey === 'task-board/task',
      'task_board_contract_mismatch', 'A conversation needs its owning ticket.');
    const prior = await this.read(current.content.value, taskId);
    const immutable = ({ nativeInput: _input, handoff: _handoff, delivery: _delivery, recipient: _recipient, ...body }: TaskBoard.Comment) => body;
    for (let index = 0; index < Math.min(prior.comments.length, task.comments.length); index++)
      requireThat(hashJson(immutable(prior.comments[index]!)) === hashJson(immutable(task.comments[index]!)),
        'task_board_comment_mismatch', 'Published comment content is immutable.');
    const receipts = new Map(task.commentDeliveries.map(receipt => [receipt.commentId, receipt]));
    const save = async (node: CommentNode): Promise<TaskBoard.ObjectPin> => {
      const hash = hashJson(node), cached = this.pins.get(hash);
      if (cached) return cached;
      const name = 'Conversation ' + hash.slice(7);
      const existing = await this.client.request('objects.query', { contractKey: commentNodeKey,
        where: { op: 'and', args: [{ field: 'object.parentId', op: 'eq', value: taskId },
          { field: 'object.name', op: 'eq', value: name }] }, limit: 1, includeArchived: true });
      let pin: TaskBoard.ObjectPin;
      if (existing.items[0]) {
        pin = { objectId: existing.items[0].objectId, revision: existing.items[0].revision };
        requireThat(hashJson(await this.reader.node(pin, taskId)) === hash,
          'task_board_evidence_mismatch', 'The immutable conversation record differs from its identity.');
      } else {
        const references = node.kind === 'page' ? Object.fromEntries(node.children.map((child, i) => ['child-' + i, child.object])) : {};
        const saved = await this.client.request('objects.write', { contractVersion: '1.0.0',
          mutationId: await this.identity('conversation:' + taskId + ':' + hash), references,
          create: { name, parentId: taskId, ownerObjectId: taskId, contractKey: commentNodeKey },
          content: { encoding: 'json', value: node as unknown as Wire.Json } });
        pin = { objectId: saved.object.id, revision: saved.revision.revision };
      }
      this.remember(hash, pin); this.reader.remember(pin, node);
      return pin;
    };
    let level: CommentIndex[] = [];
    for (let offset = 0; offset < task.comments.length; offset += 8) {
      const batch = await Promise.all(task.comments.slice(offset, offset + 8).map(async (comment, index) => {
        requireThat(comment.sequence === offset + index + 1, 'task_board_comment_mismatch', 'Conversation sequence must remain contiguous.');
        return { object: await save({ schemaVersion: 1, taskId, kind: 'comment',
          entry: { comment, receipt: receipts.get(comment.commentId) ?? null } }), length: 1 };
      }));
      level.push(...batch);
    }
    while (level.length > 1) {
      const parents: CommentIndex[] = [];
      for (let offset = 0; offset < level.length; offset += 64) {
        const children = level.slice(offset, offset + 64);
        parents.push({ object: await save({ schemaVersion: 1, taskId, kind: 'page', children }),
          length: children.reduce((sum, child) => sum + child.length, 0) });
      }
      level = parents;
    }
    return { ...task, comments: [], commentDeliveries: [],
      commentStore: { kind: 'tree', root: level[0] ?? null, length: task.comments.length,
        ...(contractVersion === '2.3.0' ? { summary: commentSummary(task, taskId) } : {}) } };
  }

  private async writeRetained(task: TaskBoard.Task, taskId: string, scope: string, contractVersion: string): Promise<StoredTask> {
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
      const saved = await this.client.request('objects.write', { mutationId, contractVersion,
        content: { encoding: 'json', value: value as unknown as Wire.Json }, references: {},
        ...(existing ? { objectId: existing.object.id, expectedRevision: existing.revision.revision } :
          { create: { name: 'Comments', parentId: taskId, ownerObjectId: null, contractKey: commentContractKey } }) });
      objectId = saved.object.id;
    }
    return taskHeader(task, objectId!);
  }
}
