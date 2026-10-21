import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, readFile, rm } from 'node:fs/promises';
import { join, relative, resolve } from 'node:path';
import { tmpdir } from 'node:os';
import { createServer } from 'node:net';
import { randomUUID } from 'node:crypto';
import { spawn } from 'node:child_process';
import { once } from 'node:events';
import { HiveServer } from '../services/hive/src/server.js';
import { HiveClient, discover, callBound, newOperationId } from '../packages/sdk/src/client.js';
import { digest, hashJson } from '../packages/contracts/src/canonical.js';
import { IvyError } from '../packages/contracts/src/errors.js';
import type { Host, Operation, TaskBoard, Wire } from '../packages/contracts/src/generated.js';
import { atomicJson } from '../packages/host-runtime/src/config.js';
import { startTaskBoard } from '../services/task-board/src/main.js';
import { retireArtifacts } from '../services/task-board/src/retire-artifacts.js';
import { fields } from './fixtures/task-board.js';
import { until } from './fixtures/host.js';

test('actual TaskBoard service serves manual tasks and retains caller outcomes across restart', async t => {
  const root = await mkdtemp(join(tmpdir(), 'ivy-task-board-service-')), cleanups: (() => Promise<unknown>)[] = [];
  t.after(async () => { for (const cleanup of cleanups.reverse()) await cleanup(); assert.ok(relative(tmpdir(), root).startsWith('ivy-task-board-service-')); await rm(root, { recursive: true, force: true }); });
  const listener = createServer(); await new Promise<void>(resolve => listener.listen(0, '127.0.0.1', resolve));
  const port = (listener.address() as { port: number }).port; await new Promise<void>(resolve => listener.close(() => resolve()));
  const base = `http://127.0.0.1:${port}/ivy`, token = 'fixture-task-board-service-token', userToken = 'fixture-task-board-user-token', workerToken = 'fixture-task-board-worker-token';
  const hive = new HiveServer({ filename: join(root, 'hive.sqlite'), publicBaseUrl: base, listenHost: '127.0.0.1', listenPort: port,
    version: 'test', buildId: digest('task-board-hive-test'), credentials: [{ principalId: 'task-board-owner', digest: digest(token) }, { principalId: 'user', digest: digest(userToken) }, { principalId: 'worker', digest: digest(workerToken) }] });
  await hive.start(); cleanups.push(() => hive.close());
  const build = JSON.parse(await readFile('dist/build-info.json', 'utf8')) as { version: string; buildId: string };
  const settings: TaskBoard.Settings = { principalId: 'task-board-owner', rootObjectId: null, scheduler: { enabled: false, intervalMs: 1000, pageSize: 5 },
     phoneTarget: null };
  const config: Host.InstanceConfig = { schemaVersion: 1, instanceId: 'task-board-test', serviceNodeId: 'task-board-test', hostId: 'fixture-host', componentId: 'task-board', version: build.version, buildId: build.buildId,
    publicBaseUrl: base, dataRoot: join(root, 'service-state'), artifactRoot: resolve('.'), credential: token, settings };
  const path = join(root, 'task-board.json'); await atomicJson(path, config);
  const service = await startTaskBoard(path); cleanups.push(() => service.close()); await service.service.waitReady(); await until(() => service.runtime?.recovered === true);
  const connection = service.service.connection, request = connection.request.bind(connection);
  let ownerReads = 0;
  const ownerProbe = t.mock.method(connection, 'request', async (...args: Parameters<typeof request>) => {
    if (args[0] === 'serviceNodes.get') ownerReads++;
    return request(...args);
  });
  for (let index = 0; index < 10; index++) await service.runtime!.engine.verifyOwner();
  assert.equal(ownerReads, 0, 'The request queue must preserve the established connection ownership cache.');
  ownerProbe.mock.restore();
  const user = new HiveClient(base, { credential: userToken }), worker = new HiveClient(base, { credential: workerToken });
  const tools = await user.request('tools.list', { namespace: 'task-board', serviceNodeId: 'task-board-test' }); assert.ok(tools.items.some(item => item.definition.name === 'read')); assert.ok(tools.items.some(item => item.definition.name === 'list')); assert.ok(tools.items.some(item => item.definition.name === 'categories'));
  const workspace = await callBound(user, await discover(user, 'task-board.workspace', { serviceNodeId: 'task-board-test' }), {}) as TaskBoard.WorkspaceInfo;
  assert.equal(workspace.rootObjectId, null); assert.equal(workspace.principalId, 'task-board-owner'); assert.equal(workspace.callerPrincipalId, 'user');
  assert.equal(workspace.role, 'user'); assert.equal(workspace.recovered, true); assert.deepEqual(workspace.scheduler, settings.scheduler);
  const workerView = await callBound(worker, await discover(worker, 'task-board.workspace', { serviceNodeId: 'task-board-test' }), {}) as TaskBoard.WorkspaceInfo;
  assert.equal(workerView.role, 'user'); assert.equal(JSON.stringify(workspace).includes(token), false);
  for (const changed of [{ principalId: 'different-owner', rootObjectId: null, callerPrincipalId: 'user' }, { principalId: 'task-board-owner', rootObjectId: 'different-root', callerPrincipalId: 'user' }, { principalId: 'task-board-owner', rootObjectId: null, callerPrincipalId: 'another-login' }]) {
    const request = { action: 'create', operationId: randomUUID(), fields, expectedWorkspace: changed };
    await assert.rejects(callBound(user, await discover(user, 'task-board.create', { serviceNodeId: 'task-board-test' }), request, request.operationId),
      (error: unknown) => error instanceof IvyError && error.code === 'task_board_workspace_changed');
    await assert.rejects(callBound(user, await discover(user, 'task-board.operation', { serviceNodeId: 'task-board-test' }), { operationId: request.operationId }),
      (error: unknown) => error instanceof IvyError && error.code === 'not_found');
  }
  const create: TaskBoard.CreateRequest = { action: 'create', operationId: randomUUID(), fields: { ...fields, title: 'Manual service acceptance', control: 'user' } };
  const invoke = async (request: TaskBoard.ActionInput) => await callBound(user, await discover(user, 'task-board.' + request.action, { serviceNodeId: 'task-board-test' }), request as unknown as Wire.Json, request.operationId) as TaskBoard.ActionOutcome;
  const created = await invoke(create); assert.ok(created.task);
  const listed = await callBound(user, await discover(user, 'task-board.list', { serviceNodeId: 'task-board-test' }), { status: 'todo' }) as TaskBoard.TaskListResult;
  assert.ok(listed.items.some(item => item.taskId === created.task!.objectId && item.title === create.fields.title));
  const read = await callBound(user, await discover(user, 'task-board.read', { serviceNodeId: 'task-board-test' }), { task: created.task.objectId }) as TaskBoard.TaskView;
  assert.equal(read.task.fields.title, create.fields.title);
  const workerCreate = { ...create, operationId: randomUUID(), fields: { ...create.fields, title: 'Trusted service caller' } };
  const workerCreated = await callBound(worker, await discover(worker, 'task-board.create', { serviceNodeId: 'task-board-test' }), workerCreate, workerCreate.operationId) as TaskBoard.ActionOutcome;
  assert.ok(workerCreated.task);
  let accepted = created;
  for (const workflowState of ['in_progress', 'done'] as const) accepted = await invoke({ action: 'transition',
    operationId: randomUUID(), taskId: accepted.task!.objectId, expectedRevision: accepted.task!.revision, workflowState, detail: null });
  const task = await user.request('objects.read', { objectId: accepted.task!.objectId });
  assert.equal(task.content.encoding, 'json'); if (task.content.encoding !== 'json') throw new Error('Expected JSON task.');
  assert.equal((task.content.value as TaskBoard.Task).workflowState, 'done');
  const archiveOperationId = randomUUID();
  const archived = await callBound(user, await discover(user, 'task-board.archive', { serviceNodeId: 'task-board-test' }), {
    action: 'archive', operationId: archiveOperationId, taskId: accepted.task!.objectId, expectedRevision: accepted.task!.revision, archived: true,
  }, archiveOperationId) as Operation.ObjectMetadata;
  assert.equal(archived.id, accepted.task!.objectId); assert.equal(archived.effectivelyArchived, true);
  await assert.rejects(user.request('objects.archive', { objectId: accepted.task!.objectId, archived: false, mutationId: await newOperationId(user) }),
    (error: unknown) => error instanceof IvyError && error.code === 'task_board_workflow_required');
  const before = service.service.connection.generation;
  await service.close();
  const replacement = await startTaskBoard(path); cleanups.push(() => replacement.close()); await replacement.service.waitReady(); await until(() => replacement.runtime?.recovered === true);
  assert.ok(replacement.service.connection.generation > before);
  assert.deepEqual(await invoke(create), created);
  assert.deepEqual(await callBound(user, await discover(user, 'task-board.archive', { serviceNodeId: 'task-board-test' }), {
    action: 'archive', operationId: archiveOperationId, taskId: accepted.task!.objectId, expectedRevision: accepted.task!.revision, archived: true,
  }, archiveOperationId), archived, 'Archive replay retains its original metadata after restart.');
  const restoreId = randomUUID();
  const restored = await callBound(user, await discover(user, 'task-board.archive', { serviceNodeId: 'task-board-test' }), {
    action: 'archive', operationId: restoreId, taskId: accepted.task!.objectId, expectedRevision: accepted.task!.revision, archived: false,
  }, restoreId) as Operation.ObjectMetadata;
  assert.equal(restored.effectivelyArchived, false);
  assert.equal(((await user.request('objects.read', { objectId: restored.id })).content.value as TaskBoard.Task).workflowState, 'done');
  const operation = await callBound(user, await discover(user, 'task-board.operation', { serviceNodeId: 'task-board-test' }), { operationId: create.operationId }) as TaskBoard.Operation;
  assert.equal(operation.phase, 'succeeded'); assert.equal(operation.callerPrincipalId, 'user');
  await assert.rejects(callBound(worker, await discover(worker, 'task-board.operation', { serviceNodeId: 'task-board-test' }), { operationId: create.operationId }),
    (error: unknown) => error instanceof IvyError && error.code === 'not_found');
  const health = JSON.parse(await readFile(join(config.dataRoot, 'health.json'), 'utf8')) as Host.Health;
  assert.equal(health.ready, true); assert.equal(JSON.stringify(health).includes(token), false);
  const file = await replacement.runtime!.engine.store.writeAttachment(restored.id, 'archived-maintenance-file', 'archived.bin', Buffer.from('locally archived bytes'));
  const archivedFile = await user.request('objects.read', file);
  const beforeCommentArchive = await replacement.runtime!.engine.store.read('task-board/task', restored.id);
  const importedComment: TaskBoard.Comment = { commentId: 'schedule-context-test-0', sequence: 1,
    author: { principalId: 'task-board-record-upgrade', serviceNodeId: 'task-board-test', generation: 1, source: 'recovery' },
    authorKind: 'system', purpose: 'update', body: 'Imported raw metadata '.repeat(1000), requests: [], responses: [],
    attachments: [], run: null, turnId: null, replyTo: null, delivery: null,
    createdAt: beforeCommentArchive.value.updatedAt, operationId: 'schedule-context-test-0' };
  await replacement.runtime!.engine.store.write('task-board/task', { ...beforeCommentArchive.value, comments: [importedComment] },
    'seed-imported-comment', { objectId: restored.id, expectedRevision: beforeCommentArchive.pin.revision });
  await atomicJson(join(config.dataRoot, 'comment-cleanup.json'), { schemaVersion: 1, scope: 'verified-comment-archive',
    archiveManifest: { hostId: 'archive-host', path: '/archive/comments.json', contentHash: digest('comment archive') },
    remaining: [{ taskId: restored.id, comments: [{ commentId: importedComment.commentId,
      bodyHash: hashJson(importedComment.body), localPath: '/archive/comments.json' }] }],
    completedTasks: 0, rewrittenComments: 0, removedCharacters: 0, updatedAt: importedComment.createdAt });
  await replacement.close();
  const stopped = spawn(process.execPath, ['-e', ''], { windowsHide: true });
  await once(stopped, 'exit'); assert.ok(stopped.pid);
  const planPath = join(root, 'archive-plan.json');
  await atomicJson(planPath, { schemaVersion: 1, hostId: config.hostId, serviceNodeId: config.serviceNodeId,
    scope: 'verified-maintenance', archiveManifest: { hostId: 'archive-host', path: '/archive/manifest.json', contentHash: digest('archive') },
    taskIds: [restored.id], files: [{ object: file, contentHash: archivedFile.revision.contentHash,
      byteLength: archivedFile.revision.byteLength, localPath: '/archive/archived.bin', ownerTaskId: restored.id }], referencePages: [] });
  const beforeMaintenance = await user.request('objects.read', { objectId: restored.id });
  await retireArtifacts(path, planPath, stopped.pid);
  const job = JSON.parse(await readFile(join(config.dataRoot, 'artifact-cleanup.json'), 'utf8'));
  assert.deepEqual(job.taskIds, [restored.id]); assert.equal(job.remaining.length, 1);
  assert.deepEqual(await user.request('objects.read', { objectId: restored.id }), beforeMaintenance,
    'The stopped-owner maintenance connects through the actual registry without altering an unselected Task.');
  const resumed = await startTaskBoard(path); cleanups.push(() => resumed.close()); await resumed.service.waitReady();
  await until(async () => JSON.parse(await readFile(join(config.dataRoot, 'artifact-cleanup.json'), 'utf8')).deletedFiles === 1);
  await assert.rejects(user.request('objects.read', file), (error: unknown) => error instanceof IvyError && error.code === 'not_found');
  await until(async () => !!JSON.parse(await readFile(join(config.dataRoot, 'comment-cleanup.json'), 'utf8')).completedAt);
  const compacted = await resumed.runtime!.engine.store.read('task-board/task', restored.id);
  assert.match(compacted.value.comments[0]!.body, /archived locally/);
  assert.ok(compacted.value.comments[0]!.body.length < 512);
  assert.equal(compacted.value.workRevision, beforeCommentArchive.value.workRevision);
  assert.deepEqual(compacted.value.latestResult, beforeCommentArchive.value.latestResult);
});
