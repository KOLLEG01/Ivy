import test from 'node:test';
import assert from 'node:assert/strict';
import { taskBoardFixture, content } from './fixtures/task-board.js';
import { retireTaskAttachments, TaskBoardArtifactCleanup, type ArtifactCleanupJob } from '../services/task-board/src/runtime/artifact-retention.js';
import { TaskComments } from '../services/task-board/src/runtime/comment-store.js';
import { hashJson, IvyError } from '../packages/sdk/src/node.js';
import type { TaskBoard } from '../packages/sdk/src/node.js';
import { mkdtemp, writeFile, readFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { nativeWorkflow } from './fixtures/task-board-native.js';
import { retireImportedTaskComments, TaskBoardCommentCleanup, type CommentCleanupJob } from '../services/task-board/src/runtime/comment-retention.js';
import { archivedCommentNote } from '../services/task-board/src/runtime/comment-store.js';
import { upgradeTaskRecord } from '../packages/contracts/src/task-board-records.js';

test('online archival rewrites only exact exported import bodies while preserving an active Run and ordinary comments', async t => {
  const flow = await nativeWorkflow(t); await flow.running();
  const store = flow.f.first.store, current = await store.read('task-board/task', flow.state().task.objectId);
  const original: TaskBoard.Comment = { commentId: 'human', sequence: 1,
    author: { ...flow.f.first.owner, principalId: 'user', source: 'user' }, authorKind: 'user', purpose: 'update',
    body: 'Keep my exact text, including JSON: {"result":"important"}.', requests: [], responses: [],
    attachments: [], run: null, turnId: null, replyTo: null, delivery: null,
    createdAt: current.value.updatedAt, operationId: 'human' };
  const imported: TaskBoard.Comment = { ...original, commentId: 'schedule-context-import-48000', sequence: 2,
    author: { ...original.author, principalId: 'task-board-record-upgrade', source: 'recovery' },
    authorKind: 'system', body: '{"technical":"' + 'old artifact metadata '.repeat(1000) + '"}' };
  await store.write('task-board/task', { ...current.value, comments: [original, imported] }, 'imported-dump',
    { objectId: current.pin.objectId, expectedRevision: current.pin.revision });
  const before = await store.read('task-board/task', current.pin.objectId);
  const archive = { commentId: imported.commentId, bodyHash: hashJson(imported.body), localPath: 'C:/archive/original.json' };
  await assert.rejects(retireImportedTaskComments(store, current.pin.objectId,
    [{ ...archive, bodyHash: hashJson('wrong') }], 'wrong-export'), e => e instanceof IvyError && e.code === 'task_board_evidence_mismatch');
  const directory = await mkdtemp(join(tmpdir(), 'ivy-comment-retirement-'));
  t.after(() => rm(directory, { recursive: true, force: true }));
  const statePath = join(directory, 'job.json');
  await writeFile(statePath, JSON.stringify({ schemaVersion: 1, scope: 'verified-comments',
    archiveManifest: { hostId: 'archive', path: 'C:/archive/manifest.json', contentHash: hashJson(archive) },
    remaining: [{ taskId: current.pin.objectId, comments: [archive] }], completedTasks: 0, rewrittenComments: 0,
    removedCharacters: 0, updatedAt: current.value.updatedAt } satisfies CommentCleanupJob));
  const cleanup = new TaskBoardCommentCleanup(store, statePath);
  await cleanup.pass(); await cleanup.stop();
  const after = await store.read('task-board/task', current.pin.objectId);
  assert.deepEqual(after.value, { ...before.value, comments: [original, { ...imported, body: archivedCommentNote(archive) }] });
  assert.deepEqual(after.value.claim, before.value.claim);
  assert.deepEqual(after.value.lastRun, before.value.lastRun);
  assert.equal(after.value.workRevision, before.value.workRevision);
  assert.equal((await store.read('task-board/run', flow.state().run.objectId)).value.phase, 'running');
  const job = JSON.parse(await readFile(statePath, 'utf8')) as CommentCleanupJob;
  assert.equal(job.rewrittenComments, 1); assert.equal(job.remaining.length, 0); assert.ok(job.completedAt);
  assert.equal((await retireImportedTaskComments(store, current.pin.objectId, [archive], 'verified-comments')).rewritten, 0);
  await assert.rejects(retireImportedTaskComments(store, current.pin.objectId,
    [{ ...archive, commentId: original.commentId, bodyHash: hashJson(original.body) }], 'forbidden-human'),
  e => e instanceof IvyError && e.code === 'task_board_evidence_mismatch');
  const comments = new TaskComments(store.client, id => store.preparedMutationId(id));
  await assert.rejects(comments.write({ ...after.value, comments: [original, { ...after.value.comments[1]!, body: 'new arbitrary text' }] },
    current.pin.objectId, 'ordinary-comment-edit'), e => e instanceof IvyError && e.code === 'task_board_comment_mismatch');
});

test('record upgrades keep imported result inventories out of conversation bodies', async t => {
  const f = taskBoardFixture(t), created = await f.create();
  const task = (await f.first.store.read('task-board/task', created.task!.objectId)).value;
  const pin = { objectId: 'old-result', revision: 1 };
  const legacy = { ...task, nextSteps: [{ id: 'owned-work', scope: 'Continue the actual owned implementation.', requires: [] }],
    deliveries: Array.from({ length: 80 }, (_, index) => ({ result: pin, delivery: { artifacts: [],
      codeReferences: [{ kind: 'commit', commit: 'a'.repeat(40), label: 'Long technical evidence '.repeat(30), url: 'https://example.com/' + index }] } })) };
  const upgraded = upgradeTaskRecord(legacy as never);
  assert.ok(upgraded.comments.every(comment => comment.body.length < 4096));
  assert.ok(upgraded.comments.every(comment => !comment.body.includes('codeReferences')));
  assert.ok(upgraded.comments.some(comment => comment.body.includes('80 saved results')));
  assert.ok(upgraded.comments.some(comment => comment.body.includes('Continue the actual owned implementation.')));
  assert.deepEqual(upgradeTaskRecord(upgraded), upgraded);
});

test('artifact retirement preserves conversation, scheduling and current result evidence without waking work', async t => {
  const f = taskBoardFixture(t), created = await f.create(), store = f.first.store;
  const original = await store.read('task-board/task', created.task!.objectId);
  const actor = { ...f.first.owner, principalId: 'user', source: 'user' as const };
  const artifacts: TaskBoard.TaskAttachment[] = [];
  for (const name of ['old-source.zip', 'current-result.bin']) {
    const pin = await store.writeAttachment(original.pin.objectId, 'upload-' + name, name, Buffer.from(name.repeat(100)));
    const read = await store.client.request('objects.read', pin);
    artifacts.push({ attachmentId: name, filename: name, object: pin, contentHash: read.revision.contentHash,
      byteLength: read.revision.byteLength, mediaType: read.revision.mediaType, uploader: actor, createdAt: original.value.createdAt });
  }
  const current = artifacts[1]!;
  const result = await store.write('task-board/result', { schemaVersion: 1, taskId: original.pin.objectId, run: null,
    kind: 'intermediate', content: { ...content, artifacts: [{ object: current.object, contentHash: current.contentHash,
      mediaType: current.mediaType, label: current.filename }] }, actor, createdAt: original.value.createdAt, operationId: 'current-proof' },
  'current-proof', { create: { parentId: store.rootObjectId, name: 'Current result' } });
  const comment: TaskBoard.Comment = { commentId: 'comment-1', sequence: 1, author: actor, authorKind: 'user',
    body: 'Preserve exact text, links and Unicode: ä 🚀.', requests: [], responses: [], delivery: null, attachments: artifacts,
    run: null, turnId: null, replyTo: null, createdAt: original.value.createdAt, operationId: 'comment-1' };
  await store.write('task-board/task', { ...original.value, comments: [comment], attachments: artifacts, latestResult: result,
    wakeAt: '2099-01-01T00:00:00.000Z' }, 'attach-source', { objectId: original.pin.objectId, expectedRevision: original.pin.revision });
  const before = await store.read('task-board/task', original.pin.objectId);
  const archives = artifacts.map(item => ({ object: item.object, contentHash: item.contentHash, byteLength: item.byteLength,
    localPath: 'C:/archive/' + item.filename }));
  await assert.rejects(retireTaskAttachments(store, original.pin.objectId,
    [{ ...archives[0]!, contentHash: hashJson('wrong') }], 'bad-archive'), e => e instanceof IvyError && e.code === 'task_board_evidence_mismatch');
  const outcome = await retireTaskAttachments(store, original.pin.objectId, archives, 'verified-export');
  assert.equal(outcome.removed, 1);
  const after = await store.read('task-board/task', original.pin.objectId);
  assert.deepEqual(after.value, { ...before.value, attachments: [current], comments: [{ ...comment, attachments: [current] }] });
  assert.deepEqual(after.value.latestResult, result);
  assert.equal((await store.page('task-board/run')).items.length, 0);
  assert.equal((await retireTaskAttachments(store, original.pin.objectId, archives, 'verified-export')).removed, 0);
  await assert.rejects(store.client.request('objects.delete', { objectId: artifacts[0]!.object.objectId, expectedRevision: 1,
    mutationId: await store.preparedMutationId('still-historically-pinned') }), e => e instanceof IvyError && e.code === 'target_conflict');
  const comments = new TaskComments(store.client, value => store.preparedMutationId(value));
  await assert.rejects(comments.retireAttachments({ ...after.value, comments: [{ ...after.value.comments[0]!, body: 'altered' }] },
    original.pin.objectId, 'forbidden-edit', new Set()), e => e instanceof IvyError && e.code === 'task_board_comment_mismatch');
  await assert.rejects(comments.retireAttachments({ ...after.value, comments: [] }, original.pin.objectId,
    'forbidden-truncate', new Set()), e => e instanceof IvyError && e.code === 'task_board_comment_mismatch');
  const protection = await store.client.request('objects.write', { contractVersion: '1.0.0',
    mutationId: await store.preparedMutationId('active-source-protection'), references: { source: before.pin },
    create: { contractKey: 'task-board/reference-page', parentId: original.pin.objectId, ownerObjectId: original.pin.objectId, name: 'Pinned run source' },
    content: { encoding: 'json', value: { schemaVersion: 1, taskId: original.pin.objectId, pins: [before.pin] } } });
  const directory = await mkdtemp(join(tmpdir(), 'ivy-artifact-retirement-'));
  t.after(() => rm(directory, { recursive: true, force: true }));
  const statePath = join(directory, 'job.json');
  const job: ArtifactCleanupJob = { schemaVersion: 1, scope: 'verified-export',
    archiveManifest: { hostId: 'archive-host', path: 'C:/archive/manifest.json', contentHash: hashJson(archives) },
    taskIds: [original.pin.objectId], referencePages: [], remaining: [archives[0]!],
    deletedFiles: 0, deletedBytes: 0, prunedRevisions: 0, updatedAt: original.value.updatedAt };
  await writeFile(statePath, JSON.stringify(job));
  const cleanup = new TaskBoardArtifactCleanup(store, statePath);
  await cleanup.pass();
  assert.equal(JSON.parse(await readFile(statePath, 'utf8')).remaining.length, 1);
  assert.equal((await store.client.request('objects.read', before.pin)).revision.revision, before.pin.revision);
  await store.client.request('objects.delete', { objectId: protection.object.id, expectedRevision: 1,
    mutationId: await store.preparedMutationId('release-completed-source') });
  await cleanup.pass();
  const finished = JSON.parse(await readFile(statePath, 'utf8')) as ArtifactCleanupJob;
  assert.equal(finished.remaining.length, 0);
  assert.equal(finished.deletedFiles, 1);
  assert.equal(finished.deletedBytes, archives[0]!.byteLength);
  assert.ok(finished.completedAt);
  await assert.rejects(store.client.request('objects.read', artifacts[0]!.object), e => e instanceof IvyError && e.code === 'not_found');
  assert.deepEqual((await store.read('task-board/task', original.pin.objectId)).value, after.value);
  assert.equal((await store.client.request('objects.read', current.object)).revision.contentHash, current.contentHash);
  await cleanup.stop();
});

test('retirement releases obsolete source pins from completed legacy Run revisions', async t => {
  const flow = await nativeWorkflow(t); await flow.running();
  const store = flow.f.first.store;
  let task = await store.read('task-board/task', flow.state().task.objectId);
  const file = await store.writeAttachment(task.pin.objectId, 'legacy-file', 'source.zip', Buffer.from('archived source'));
  const savedFile = await store.client.request('objects.read', file);
  const attachment: TaskBoard.TaskAttachment = { attachmentId: 'legacy-file', filename: 'source.zip', object: file,
    contentHash: savedFile.revision.contentHash, byteLength: savedFile.revision.byteLength, mediaType: savedFile.revision.mediaType,
    uploader: { ...flow.f.first.owner, principalId: 'user', source: 'user' }, createdAt: task.value.createdAt };
  await store.write('task-board/task', { ...task.value, attachments: [attachment] }, 'legacy-attachment',
    { objectId: task.pin.objectId, expectedRevision: task.pin.revision });
  task = await store.read('task-board/task', task.pin.objectId);
  const running = (await store.read('task-board/run', flow.state().run.objectId)).value;
  const originalRequest = { ...running.originalRequest, expectedRevision: task.pin.revision };
  const legacy = await store.client.request('objects.write', { contractVersion: '3.0.0',
    mutationId: await store.preparedMutationId('legacy-running-source'), references: { 'source-task': task.pin },
    create: { contractKey: 'task-board/run', name: 'Legacy primary Run', parentId: store.rootObjectId, ownerObjectId: null },
    content: { encoding: 'json', value: { ...running, originalRequest, requestHash: hashJson(originalRequest) } } });
  const archive = { object: file, contentHash: savedFile.revision.contentHash, byteLength: savedFile.revision.byteLength,
    localPath: '/archive/source.zip' };
  await retireTaskAttachments(store, task.pin.objectId, [archive], 'legacy-retirement');
  await flow.handoff(); await flow.update((await flow.snapshot()).change);
  const completed = (await store.read('task-board/run', flow.state().run.objectId)).value;
  await store.client.request('objects.write', { objectId: legacy.object.id, expectedRevision: legacy.revision.revision,
    contractVersion: '3.0.0', mutationId: await store.preparedMutationId('legacy-completed-source'), references: {},
    content: { encoding: 'json', value: { ...completed, originalRequest, requestHash: hashJson(originalRequest) } } });
  const directory = await mkdtemp(join(tmpdir(), 'ivy-legacy-retirement-')); t.after(() => rm(directory, { recursive: true, force: true }));
  const statePath = join(directory, 'job.json');
  await writeFile(statePath, JSON.stringify({ schemaVersion: 1, scope: 'legacy-retirement',
    archiveManifest: { hostId: 'archive', path: '/archive/manifest.json', contentHash: hashJson(archive) },
    taskIds: [task.pin.objectId], referencePages: [], remaining: [archive], deletedFiles: 0, deletedBytes: 0,
    prunedRevisions: 0, updatedAt: task.value.createdAt } satisfies ArtifactCleanupJob));
  const cleanup = new TaskBoardArtifactCleanup(store, statePath);
  await cleanup.pass(); await cleanup.pass();
  assert.equal(JSON.parse(await readFile(statePath, 'utf8')).deletedFiles, 1);
  assert.equal((await store.client.request('objects.read', { objectId: legacy.object.id })).revision.revision, 2);
  await assert.rejects(store.client.request('objects.read', file), e => e instanceof IvyError && e.code === 'not_found');
  await cleanup.stop();
});
