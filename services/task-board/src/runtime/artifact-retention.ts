import { hashJson, requireThat } from '../../../../packages/sdk/src/node.js';
import type { Operation, TaskBoard, Wire } from '../../../../packages/sdk/src/node.js';
import { TaskComments, validateTaskContent } from './comment-store.js';
import { TaskBoardChecks } from './checks.js';
import { contractVersion, validateDomain } from './schema.js';
import { taskReferences } from './reference-pages.js';
import { taskAttachmentPins, type TaskBoardStore } from './store.js';
import { readFile, rename, writeFile } from 'node:fs/promises';

export interface ArchivedAttachment {
  object: TaskBoard.ObjectPin;
  contentHash: string;
  byteLength: number;
  localPath: string;
}

export async function pendingPublication(store: TaskBoardStore, taskId: string): Promise<boolean> {
  const page = await store.page('task-board/operation', { limit: 1, where: { op: 'and', args: [
    { field: 'data:/request/taskId', op: 'eq', value: taskId },
    { field: 'data:/phase', op: 'in', value: ['accepted', 'preparing', 'publishing', 'needs_attention'] },
  ] } });
  return page.items.length > 0;
}

/** Owner-only maintenance. The caller first verifies its durable local export and quiesces publications. */
export async function retireTaskAttachments(store: TaskBoardStore, taskId: string,
  archives: ArchivedAttachment[], scope: string): Promise<{ task: TaskBoard.ObjectPin; removed: number }> {
  const previous = await store.read('task-board/task', taskId, true, true);
  requireThat(!previous.value.publication && !await pendingPublication(store, taskId),
    'task_board_publication_busy', 'Finish the original publication before retiring attachments.');
  const index = new Map(archives.map(item => [hashJson(item.object), item]));
  requireThat(index.size === archives.length, 'invalid_arguments', 'An archive inventory must contain unique revision pins.');
  for (const item of archives) requireThat(item.localPath.length > 0 && /^(?:[A-Za-z]:[\\/]|\/)/.test(item.localPath),
    'invalid_arguments', 'Every retired attachment needs an absolute archive path.');
  const protectedPins = new Set<string>();
  if (previous.value.latestResult) {
    const result = await store.read('task-board/result', previous.value.latestResult, true, true);
    for (const pin of Object.values(result.revision.references)) protectedPins.add(hashJson(pin));
  }
  const retiring = new Set<string>();
  for (const attachment of [...previous.value.attachments, ...previous.value.comments.flatMap(comment => comment.attachments)]) {
    const identity = hashJson(attachment.object), archive = index.get(identity);
    if (!archive || protectedPins.has(identity)) continue;
    requireThat(archive.contentHash === attachment.contentHash && archive.byteLength === attachment.byteLength,
      'task_board_evidence_mismatch', 'The verified archive must match the exact published attachment.');
    retiring.add(identity);
  }
  if (!retiring.size) return { task: previous.pin, removed: 0 };
  const next: TaskBoard.Task = { ...previous.value,
    attachments: previous.value.attachments.filter(item => !retiring.has(hashJson(item.object))),
    comments: previous.value.comments.map(comment => ({ ...comment,
      attachments: comment.attachments.filter(item => !retiring.has(hashJson(item.object))) })) };
  validateDomain('task-board/task', next);
  validateTaskContent(next, taskId);
  await new TaskBoardChecks(store).aggregate(next, taskId);
  const comments = new TaskComments(store.client, identity => store.preparedMutationId(scope + ':' + identity));
  const stored = await comments.retireAttachments(next, taskId, scope, retiring, contractVersion('task-board/task'));
  const named = Object.fromEntries(Object.entries(previous.revision.references)
    .filter(([name]) => !/^(attachment-|reference-page-)/.test(name) && name !== 'conversation'));
  if (stored.commentStore.root) named['conversation'] = stored.commentStore.root.object;
  const references = await taskReferences(store.client, taskId, named, taskAttachmentPins(next),
    identity => store.preparedMutationId(scope + ':' + identity));
  const params: Operation.ObjectsWriteParams = { objectId: taskId, expectedRevision: previous.pin.revision,
    expectedArchived: previous.metadata.effectivelyArchived, contractVersion: contractVersion('task-board/task'), references,
    mutationId: await store.preparedMutationId(scope + ':task:' + hashJson(previous.pin)),
    content: { encoding: 'json', value: stored as unknown as Wire.Json } };
  // No workflow intent, comment, workRevision increment or notification is generated.
  const saved = await store.client.request('objects.write', params);
  return { task: { objectId: saved.object.id, revision: saved.revision.revision }, removed: retiring.size };
}

export interface ArtifactCleanupJob {
  schemaVersion: 1;
  scope: string;
  archiveManifest: { hostId: string; path: string; contentHash: string };
  taskIds: string[];
  referencePages: TaskBoard.ObjectPin[];
  remaining: ArchivedAttachment[];
  deletedFiles: number;
  deletedBytes: number;
  prunedRevisions: number;
  pruneAttempts?: number;
  taskCursor?: number;
  fileCursor?: number;
  runHistoryCursor?: string | null;
  updatedAt: string;
  completedAt?: string;
  lastError?: string;
}

/** A bounded owner collector releases old pins as Runs finish; it never changes a Task or schedules agent work. */
export class TaskBoardArtifactCleanup {
  private timer: NodeJS.Timeout | undefined;
  private active: Promise<void> | undefined;
  private stopped = false;
  private scanPasses = 0;
  private scanProgress = 0;
  private idle = false;
  private job: ArtifactCleanupJob | null = null;
  constructor(readonly store: TaskBoardStore, readonly statePath: string) {}
  start(): void { if (!this.stopped && !this.active && !this.timer) this.tick(); }
  private tick(): void {
    if (this.stopped || this.active) return;
    this.timer = undefined;
    let delay = 30_000;
    this.active = this.pass().then(progress => {
      this.scanPasses++; this.scanProgress += progress;
      if (progress) this.idle = false;
      if (!this.job?.runHistoryCursor && this.scanPasses >= (this.job?.taskIds.length ?? 1)) {
        this.idle = this.scanProgress === 0; this.scanPasses = 0; this.scanProgress = 0;
      }
      if (!this.idle) delay = 1000;
    }).catch(async error => {
      if (this.job) {
        this.job.lastError = String((error as Error).message).slice(0, 1000);
        await this.save().catch(() => undefined);
      }
    }).finally(() => {
      this.active = undefined;
      if (!this.stopped && !this.job?.completedAt) this.timer = setTimeout(() => this.tick(), delay);
    });
  }
  async stop(): Promise<void> { this.stopped = true; if (this.timer) clearTimeout(this.timer); await this.active; }
  async pass(limit = 24): Promise<number> {
    if (!this.job) {
      const text = await readFile(this.statePath, 'utf8').catch(error => {
        if ((error as NodeJS.ErrnoException).code === 'ENOENT') return null; throw error;
      });
      if (!text) return 0;
      const job = JSON.parse(text) as ArtifactCleanupJob;
      requireThat(job.schemaVersion === 1 && Array.isArray(job.taskIds) && Array.isArray(job.remaining) &&
        Array.isArray(job.referencePages) && typeof job.scope === 'string' && !!job.archiveManifest?.contentHash,
      'invalid_arguments', 'Invalid attachment cleanup job.');
      this.job = job;
    }
    const job = this.job;
    if (job.completedAt || this.stopped) return 0;
    let progress = 0;
    if (job.taskIds.length) {
      const index = (job.taskCursor ?? 0) % job.taskIds.length, taskId = job.taskIds[index]!;
      job.taskCursor = index + 1;
      if (!await pendingPublication(this.store, taskId)) {
        const task = await this.store.client.request('objects.stat', { objectId: taskId });
        requireThat(task.contractKey === 'task-board/task' && task.parentId === this.store.rootObjectId,
          'task_board_scope_mismatch', 'Cleanup history must belong to this TaskBoard workspace.');
        progress += await this.pruneHistory(taskId, task.currentRevision, job);
      }
      // Terminal legacy Runs can retain obsolete source Task pins in their old revisions.
      // Apply their existing current-only retention; incoming exact pins still protect needed snapshots.
      const runs = await this.store.client.request('objects.query', { contractKey: 'task-board/run', includeArchived: true,
        limit: 32, includeContent: true, ...(job.runHistoryCursor ? { cursor: job.runHistoryCursor } : {}) });
      for (const run of runs.items) {
        const document = run.document, value = document?.content.encoding === 'json'
          ? document.content.value as unknown as TaskBoard.Run : null;
        if (document?.object.parentId !== this.store.rootObjectId || document.object.ownerObjectId !== null ||
            !value || !job.taskIds.includes(value.taskId) || !['completed', 'failed', 'cancelled'].includes(value.phase)) continue;
        progress += await this.pruneHistory(run.objectId, run.revision, job);
      }
      job.runHistoryCursor = runs.nextCursor;
    }
    for (const page of job.referencePages.slice(0, 2)) {
      if (await this.remove(page, 'task-board/reference-page', job)) {
        job.referencePages = job.referencePages.filter(item => item.objectId !== page.objectId); progress++;
      } else job.referencePages = [...job.referencePages.slice(1), page];
    }
    const selected = Math.min(limit, job.remaining.length);
    for (let index = 0; index < selected && job.remaining.length && !this.stopped; index++) {
      const position = (job.fileCursor ?? 0) % job.remaining.length, file = job.remaining[position]!;
      if (await this.remove(file.object, 'task-board/attachment', job, file)) {
        job.remaining.splice(position, 1); job.deletedFiles++; job.deletedBytes += file.byteLength; progress++;
        job.fileCursor = position;
      } else job.fileCursor = position + 1;
    }
    job.updatedAt = new Date().toISOString();
    delete job.lastError;
    if (!job.remaining.length) job.completedAt = job.updatedAt;
    await this.save();
    return progress;
  }
  private async save(): Promise<void> {
    await writeFile(this.statePath + '.partial', JSON.stringify(this.job));
    await rename(this.statePath + '.partial', this.statePath);
  }
  private async pruneHistory(objectId: string, revision: number, job: ArtifactCleanupJob): Promise<number> {
    try {
      const pruned = await this.store.client.request('objects.pruneRevisions', { objectId, expectedRevision: revision,
        maximumCount: 1, maximumAgeDays: 1, maximumBytes: 1,
        mutationId: await this.store.preparedMutationId(job.scope + ':prune:' + objectId + ':' + revision + ':' + (job.pruneAttempts ?? 0)) });
      job.pruneAttempts = (job.pruneAttempts ?? 0) + 1;
      job.prunedRevisions += pruned.deleted;
      return pruned.deleted;
    } catch (error) { if ((error as { code?: string }).code === 'revision_conflict') return 0; throw error; }
  }
  private async remove(pin: TaskBoard.ObjectPin, key: string, job: ArtifactCleanupJob, file?: ArchivedAttachment): Promise<boolean> {
    try {
      const metadata = await this.store.client.request('objects.stat', { objectId: pin.objectId });
      requireThat(metadata.contractKey === key && !!metadata.ownerObjectId && job.taskIds.includes(metadata.ownerObjectId),
        'task_board_scope_mismatch', 'Cleanup cannot delete content outside its archived Tasks.');
      if (file) {
        requireThat(metadata.currentRevision === pin.revision, 'revision_conflict', 'The archived file changed before deletion.');
        const history = await this.store.client.request('objects.history', { objectId: pin.objectId, limit: 1 });
        requireThat(history.items[0]?.contentHash === file.contentHash && history.items[0]?.byteLength === file.byteLength,
          'task_board_evidence_mismatch', 'Cleanup cannot delete bytes that differ from the verified export.');
      }
      await this.store.client.request('objects.delete', { objectId: pin.objectId, expectedRevision: metadata.currentRevision,
        mutationId: await this.store.preparedMutationId(job.scope + ':delete:' + pin.objectId + ':' + metadata.currentRevision) });
      return true;
    } catch (error) {
      const code = (error as { code?: string }).code;
      if (code === 'not_found') return true;
      if (code === 'target_conflict' || code === 'revision_conflict') return false;
      throw error;
    }
  }
}
