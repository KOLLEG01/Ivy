import { readFile, rename, writeFile } from 'node:fs/promises';
import { hashJson, requireThat } from '../../../../packages/sdk/src/node.js';
import { archivedCommentNote, importedTechnicalComment, TaskComments, validateTaskContent } from './comment-store.js';
import type { ArchivedCommentBody } from './comment-store.js';
import type { TaskBoardStore } from './store.js';
import { validateDomain, contractVersion } from './schema.js';
import { pendingPublication } from './artifact-retention.js';

export async function retireImportedTaskComments(store: TaskBoardStore, taskId: string,
  archives: ArchivedCommentBody[], scope: string): Promise<{ rewritten: number; removedCharacters: number }> {
  const previous = await store.read('task-board/task', taskId, true, true);
  requireThat(!previous.value.publication && !await pendingPublication(store, taskId),
    'task_board_publication_busy', 'Finish the original publication before compacting imported comments.');
  requireThat(previous.metadata.parentId === store.rootObjectId && previous.metadata.ownerObjectId === null,
    'task_board_scope_mismatch', 'Comment archival must belong to this workspace.');
  const index = new Map(archives.map(archive => [archive.commentId, archive]));
  requireThat(index.size === archives.length, 'invalid_arguments', 'Archive comment identities must be unique.');
  let rewritten = 0, removedCharacters = 0;
  const next = { ...previous.value, comments: previous.value.comments.map(comment => {
    const archive = index.get(comment.commentId);
    if (!archive || comment.body === archivedCommentNote(archive)) return comment;
    requireThat(importedTechnicalComment(comment) && hashJson(comment.body) === archive.bodyHash,
      'task_board_evidence_mismatch', 'The original generated comment must match its verified archive.');
    const body = archivedCommentNote(archive);
    rewritten++; removedCharacters += comment.body.length - body.length;
    return { ...comment, body };
  }) };
  requireThat(archives.every(archive => previous.value.comments.some(comment => comment.commentId === archive.commentId)),
    'task_board_evidence_mismatch', 'An archived comment is missing from its ticket.');
  if (!rewritten) return { rewritten, removedCharacters };
  validateDomain('task-board/task', next); validateTaskContent(next, taskId);
  const comments = new TaskComments(store.client, identity => store.preparedMutationId(scope + ':' + identity));
  const stored = await comments.retireImportedBodies(next, taskId, scope, index, contractVersion('task-board/task'));
  const references = { ...previous.revision.references };
  if (stored.commentStore.root) references['conversation'] = stored.commentStore.root.object;
  await store.client.request('objects.write', { objectId: taskId, expectedRevision: previous.pin.revision,
    expectedArchived: previous.metadata.effectivelyArchived, contractVersion: contractVersion('task-board/task'), references,
    mutationId: await store.preparedMutationId(scope + ':task:' + hashJson(previous.pin)),
    content: { encoding: 'json', value: stored as never } });
  return { rewritten, removedCharacters };
}

export interface CommentCleanupJob {
  schemaVersion: 1;
  scope: string;
  archiveManifest: { hostId: string; path: string; contentHash: string };
  remaining: { taskId: string; comments: ArchivedCommentBody[] }[];
  completedTasks: number;
  rewrittenComments: number;
  removedCharacters: number;
  updatedAt: string;
  completedAt?: string;
  lastError?: string;
}

/** Local owner maintenance uses fresh CAS writes and yields to accepted workflow publications. */
export class TaskBoardCommentCleanup {
  private job: CommentCleanupJob | null = null;
  private timer: NodeJS.Timeout | undefined;
  private active: Promise<void> | undefined;
  private stopped = false;
  constructor(readonly store: TaskBoardStore, readonly statePath: string) {}
  start(): void {
    if (this.stopped || this.active || this.timer) return;
    this.active = this.pass().catch(async error => {
      if (this.job) { this.job.lastError = String((error as Error).message).slice(0, 1000); await this.save().catch(() => undefined); }
    }).finally(() => {
      this.active = undefined;
      if (!this.stopped && !this.job?.completedAt) this.timer = setTimeout(() => { this.timer = undefined; this.start(); }, 1000);
    });
  }
  async stop(): Promise<void> { this.stopped = true; if (this.timer) clearTimeout(this.timer); await this.active; }
  async pass(): Promise<void> {
    if (!this.job) {
      const job = JSON.parse(await readFile(this.statePath, 'utf8')) as CommentCleanupJob;
      requireThat(job.schemaVersion === 1 && Array.isArray(job.remaining) && typeof job.scope === 'string' &&
        !!job.archiveManifest?.contentHash, 'invalid_arguments', 'Invalid verified comment archive job.');
      this.job = job;
    }
    const job = this.job;
    if (this.stopped || job.completedAt) return;
    const selected = job.remaining[0];
    if (selected) {
      try {
        const result = await retireImportedTaskComments(this.store, selected.taskId, selected.comments, job.scope);
        job.remaining.shift(); job.completedTasks++; job.rewrittenComments += result.rewritten;
        job.removedCharacters += result.removedCharacters; delete job.lastError;
      } catch (error) {
        if (!['revision_conflict', 'task_board_publication_busy'].includes((error as { code?: string }).code ?? '')) throw error;
        job.remaining.push(job.remaining.shift()!);
      }
    }
    job.updatedAt = new Date().toISOString();
    if (!job.remaining.length) job.completedAt = job.updatedAt;
    await this.save();
  }
  private async save(): Promise<void> {
    await writeFile(this.statePath + '.partial', JSON.stringify(this.job));
    await rename(this.statePath + '.partial', this.statePath);
  }
}
