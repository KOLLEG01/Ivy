import test from 'node:test';
import assert from 'node:assert/strict';
import type { TaskBoard } from '../packages/contracts/src/generated.js';
import { content, retainedTaskFixture, taskBoardFixture } from './fixtures/task-board.js';
import { taskBoardContracts } from '../services/task-board/src/runtime/schema.js';
import { TaskComments } from '../services/task-board/src/runtime/comment-store.js';
import { hashJson, canonical, IvyError, jsonObjectContentBytes } from '../packages/sdk/src/node.js';
import { nativeWorkflow } from './fixtures/task-board-native.js';

const comment = (sequence: number): TaskBoard.Comment => ({ commentId: 'comment-' + sequence, sequence,
  author: { principalId: 'user', serviceNodeId: 'task-board', generation: 1, source: 'user' }, authorKind: 'user',
  body: 'Conversation body '.repeat(1024), requests: [], responses: [], delivery: null, attachments: [], run: null,
  turnId: null, replyTo: null, createdAt: '2026-05-01T12:00:00.000Z', operationId: 'comment-' + sequence });

test('finding migration preserves exact results, comment bodies, evidence and scheduling without duplicate notes', async context => {
  const fixture = taskBoardFixture(context), created = await fixture.create(), store = fixture.first.store;
  const original = await store.read('task-board/task', created.task!.objectId);
  const proof = await store.write('task-board/result', { schemaVersion: 1, taskId: original.pin.objectId, run: null,
    kind: 'intermediate', content, actor: { ...fixture.first.owner, principalId: 'user', source: 'user' },
    createdAt: original.value.createdAt, operationId: 'proof' }, 'proof',
  { create: { parentId: store.rootObjectId, name: 'Original verification proof' } });
  const proofRecord = await store.client.request('objects.read', proof);
  const artifact = { object: proof, label: 'Original proof', contentHash: proofRecord.revision.contentHash,
    mediaType: proofRecord.revision.mediaType };
  const findings = [
    { key: 'approval', execution: 'completed', finding: { conditionMet: false, detail: 'Review pending. Unicode 🚀 and ``` stay intact.' },
      issue: null, checkedAt: original.value.createdAt, evidence: [artifact] },
    { key: 'provider', execution: 'failed', finding: null, issue: { code: 'connection_lost', detail: 'The endpoint did not respond.' },
      checkedAt: original.value.createdAt, evidence: [artifact] },
  ];
  const retainedResult = await store.client.request('objects.write', { contractVersion: '2.0.0',
    create: { contractKey: 'task-board/result', parentId: store.rootObjectId, ownerObjectId: null, name: 'Finding-bearing result' },
    references: { proof }, mutationId: await store.preparedMutationId('retained-findings-result'),
    content: { encoding: 'json', value: { schemaVersion: 1, taskId: original.pin.objectId, run: null, kind: 'final',
      content: { ...content, checks: findings }, actor: { ...fixture.first.owner, principalId: 'user', source: 'user' },
      createdAt: original.value.createdAt, operationId: 'retained-result' } as never } });
  const resultPin = { objectId: retainedResult.object.id, revision: retainedResult.revision.revision };
  const nextSteps = [{ id: 'appointment', scope: 'Read the next provider observation.',
    requires: [{ kind: 'time' as const, at: '2099-01-01T00:00:00.000Z' }] }];
  const firstComment = { ...comment(1), authorKind: 'agent' as const, purpose: 'handoff' as const,
    handoff: { workflowState: 'waiting' as const, nextSteps, checks: findings } };
  const { wakeAt: _wake, dependencies: _dependencies, observedDependencies: _observed, ...oldTask } = original.value;
  const retainedTask = await store.client.request('objects.write', { contractVersion: '2.0.0',
    create: { contractKey: 'task-board/task', parentId: store.rootObjectId, ownerObjectId: null, name: 'Finding-bearing ticket' },
    references: { result: resultPin }, mutationId: await store.preparedMutationId('retained-findings-task'),
    content: { encoding: 'json', value: { ...oldTask, taskKey: 'TASK-8000', workflowState: 'waiting', nextSteps,
      latestChecks: findings, latestResult: resultPin, comments: [firstComment], agentCommentCount: 1 } as never } });
  for (let pass = 0; pass < 5 && !await store.compactComments(100); pass++) {}
  const migrated = await store.read('task-board/task', retainedTask.object.id);
  assert.equal('latestChecks' in migrated.value, false);
  assert.equal('checks' in migrated.value.comments[0]!.handoff!, false);
  assert.equal(migrated.value.comments[0]!.body, firstComment.body);
  assert.equal(migrated.value.comments.length, 4);
  const notes = migrated.value.comments.slice(1, 3);
  assert.match(migrated.value.comments[3]!.body, /Read the next provider observation/);
  assert.ok(notes.every(note => note.authorKind === 'system' && note.body.includes('Review pending.') && note.body.includes('connection_lost')));
  assert.ok(notes.flatMap(note => note.attachments).every(attachment => attachment.byteLength === proofRecord.revision.byteLength));
  assert.equal(migrated.value.wakeAt, '2099-01-01T00:00:00.000Z');
  assert.deepEqual(migrated.value.latestResult, resultPin);
  assert.deepEqual(migrated.value.commentDeliveries, original.value.commentDeliveries);
  assert.equal(migrated.value.workRevision, original.value.workRevision);
  assert.equal(migrated.value.agentCommentCount, 1);
  assert.equal(migrated.value.workflowState, 'waiting');
  assert.equal((await store.page('task-board/run')).items.length, 0);
  assert.equal(await store.compactComments(100), true);
  assert.equal((await store.read('task-board/task', retainedTask.object.id)).value.comments.length, 4);
  const result = await store.read('task-board/result', retainedResult.object.id);
  assert.equal('checks' in result.value.content, false);
  assert.ok(result.value.content.summary.startsWith(content.summary));
  const summary = result.value.content.summary;
  assert.deepEqual(JSON.parse(summary.slice(summary.indexOf('json\n') + 5, summary.lastIndexOf('\n'))), findings);
  assert.equal(result.value.content.artifacts.length, 1);
  assert.deepEqual(result.value.content.artifacts[0]!.object, proof);
  const raw = await store.client.request('objects.read', { objectId: retainedTask.object.id });
  assert.equal(raw.revision.contractVersion, '3.0.0');
  assert.equal('latestChecks' in (raw.content as { value: object }).value, false);
});

test('finding-bearing results migrate while an active ticket waits for safe storage backfill', async context => {
  const workflow = await nativeWorkflow(context); await workflow.running();
  const store = workflow.f.first.store;
  const task = await store.read('task-board/task', workflow.state().task.objectId);
  const { wakeAt: _wake, dependencies: _dependencies, observedDependencies: _observed, ...oldTask } = task.value;
  const legacy = await store.client.request('objects.write', { create: { contractKey: 'task-board/task',
    parentId: store.rootObjectId, ownerObjectId: null, name: 'Active legacy ticket' },
    contractVersion: '2.0.0', mutationId: await store.preparedMutationId('active-legacy-ticket'),
    references: task.revision.references, content: { encoding: 'json', value: { ...oldTask, taskKey: 'TASK-8001', nextSteps: [], latestChecks: [] } as never } });
  const result = await store.client.request('objects.write', { create: { contractKey: 'task-board/result',
    parentId: store.rootObjectId, ownerObjectId: null, name: 'Active ticket finding' }, contractVersion: '2.0.0', references: {},
    mutationId: await store.preparedMutationId('active-ticket-finding'), content: { encoding: 'json', value: {
      schemaVersion: 1, taskId: legacy.object.id, run: task.value.lastRun, kind: 'intermediate',
      content: { ...content, checks: [{ key: 'provider', execution: 'completed', finding: { conditionMet: false, detail: 'Pending.' },
        issue: null, checkedAt: task.value.updatedAt, evidence: [] }] }, actor: task.value.claim!.owner,
      createdAt: task.value.updatedAt, operationId: 'active-ticket-result',
    } as never } });
  assert.equal(await store.compactComments(100), false);
  const migrated = await store.client.request('objects.read', { objectId: result.object.id });
  assert.equal(migrated.revision.contractVersion, '2.2.0');
  assert.ok(migrated.content.encoding === 'json');
  assert.equal('checks' in (migrated.content.value as unknown as TaskBoard.Result).content, false);
  assert.ok((await store.read('task-board/result', result.object.id)).value.content.summary.includes('Pending.'));
  const unchanged = await store.client.request('objects.read', { objectId: legacy.object.id });
  assert.equal(unchanged.revision.revision, legacy.revision.revision);
  assert.deepEqual((await store.read('task-board/task', legacy.object.id)).value.claim, task.value.claim);
});

test('a finding-bearing prepared write replays its exact accepted identity after a lost reply', async context => {
  const fixture = taskBoardFixture(context), created = await fixture.create(), store = fixture.first.store;
  const task = await store.read('task-board/task', created.task!.objectId);
  const operation = await fixture.first.operations.begin({ action: 'edit', operationId: 'legacy-edit', taskId: task.pin.objectId,
    expectedRevision: task.pin.revision, fields: task.value.fields },
  { ...fixture.first.owner, principalId: 'user', source: 'user' });
  const value = { schemaVersion: 1 as const, taskId: task.pin.objectId, run: null, kind: 'intermediate' as const,
    content: { ...content, checks: [{ key: 'approval', execution: 'completed', finding: { conditionMet: false, detail: 'Pending.' },
      issue: null, checkedAt: task.value.updatedAt, evidence: [] }] }, actor: { ...fixture.first.owner, principalId: 'user', source: 'user' as const },
    createdAt: task.value.updatedAt, operationId: 'original-result' };
  const payload = await store.writePrepared('task-board/result', value, await store.preparedMutationId('legacy-payload'),
    { create: { parentId: operation.pin.objectId, name: 'Legacy prepared payload' } }, '2.0.0');
  const entry: TaskBoard.PreparedWrite = { contractKey: 'task-board/result', contractVersion: '2.0.0',
    mutationId: await store.preparedMutationId('legacy-apply'), payload: { object: payload, label: 'Legacy prepared payload',
      contentHash: hashJson(value), mediaType: 'application/json' },
    destination: { create: { parentId: store.rootObjectId, name: 'Legacy accepted result' } }, outcome: null };
  await store.write('task-board/operation', { ...operation.value, phase: 'preparing', writes: [entry] }, 'legacy-prepared-stage',
    { objectId: operation.pin.objectId, expectedRevision: operation.pin.revision });
  let loseReply = true;
  fixture.intercept(params => {
    if ('create' in params && params.create.name === 'Legacy accepted result' && loseReply) {
      loseReply = false; throw new IvyError('transport_lost', 'The accepted write reply was lost.', 'unknown');
    }
  });
  await assert.rejects(fixture.first.operations.apply(operation.pin.objectId, 0), { code: 'transport_lost' });
  const applied = await fixture.first.operations.apply(operation.pin.objectId, 0);
  const saved = await store.client.request('objects.read', applied);
  assert.equal(saved.revision.contractVersion, '2.0.0');
  assert.equal(saved.revision.contentHash, entry.payload.contentHash);
  assert.deepEqual(saved.content, { encoding: 'json', value });
  assert.deepEqual(await fixture.first.operations.apply(operation.pin.objectId, 0), applied);
  const original = await store.read('task-board/operation', operation.pin.objectId);
  assert.equal(original.value.requestHash, operation.value.requestHash);
  assert.deepEqual(original.value.writes[0]!.payload, entry.payload);
});

test('a native handoff completes when its separate ticket and conversation fit but their combined view exceeds one Object', async t => {
  const workflow = await nativeWorkflow(t);
  await workflow.running();
  const store = workflow.f.first.store;
  const current = await store.read('task-board/task', workflow.state().task);
  await store.write('task-board/task', { ...current.value,
    fields: { ...current.value.fields, description: 'Acceptance context '.repeat(12000) },
    comments: Array.from({ length: 55 }, (_entry, index) => comment(index + 1)) }, 'large-conversation',
  { objectId: current.pin.objectId, expectedRevision: current.pin.revision });
  await workflow.refresh();
  await workflow.handoff();
  await workflow.update((await workflow.snapshot()).change);
  const completed = await store.read('task-board/task', current.pin.objectId);
  assert.equal(completed.value.workflowState, 'review');
  assert.equal(completed.value.claim, null);
  assert.equal(completed.value.completionIssue, undefined);
  assert.ok(Buffer.byteLength(canonical(completed.value)) > jsonObjectContentBytes);
  const header = await store.client.request('objects.read', { objectId: current.pin.objectId });
  assert.ok(header.revision.byteLength < jsonObjectContentBytes);
  const records = await store.client.request('objects.query', { contractKey: 'task-board/comment-node', limit: 2 });
  const conversation = await store.client.request('objects.read', { objectId: records.items[0]!.objectId });
  assert.ok(conversation.revision.byteLength < jsonObjectContentBytes);
  assert.equal(completed.value.comments.length, 56);
});

test('ticket status revisions share immutable conversation records and retain exact comment prefixes', async t => {
  const f = taskBoardFixture(t), created = await f.create();
  const store = f.first.store, taskId = created.task!.objectId;
  const initial = await store.read('task-board/task', taskId);
  const first = await store.write('task-board/task', { ...initial.value, comments: [comment(1)] }, 'first-comment',
    { objectId: taskId, expectedRevision: initial.pin.revision });
  const loaded = await store.read('task-board/task', first);
  assert.deepEqual(loaded.value.comments, [comment(1)]);
  const status = await store.write('task-board/task', { ...loaded.value, workRevision: loaded.value.workRevision + 1 }, 'status-only',
    { objectId: taskId, expectedRevision: first.revision });
  const objects = await store.client.request('objects.query', { contractKey: 'task-board/comment-node', limit: 10 });
  assert.equal(objects.items.length, 1);
  const log = await store.client.request('objects.read', { objectId: objects.items[0]!.objectId });
  assert.equal(log.object.currentRevision, 1, 'status updates reuse the immutable conversation');
  const raw = await store.client.request('objects.read', { objectId: taskId });
  assert.ok(raw.revision.byteLength < comment(1).body.length / 2, 'the ticket header must not contain comment bodies');
  const current = await store.read('task-board/task', status);
  await store.write('task-board/task', { ...current.value, comments: [...current.value.comments, comment(2)] }, 'second-comment',
    { objectId: taskId, expectedRevision: status.revision });
  assert.deepEqual((await store.read('task-board/task', first)).value.comments, [comment(1)]);
  assert.deepEqual((await store.read('task-board/task', taskId)).value.comments, [comment(1), comment(2)]);
  const contracts = taskBoardContracts();
  assert.deepEqual(contracts.find(c => c.key === 'task-board/task' && c.version === '1.10.0')!.retention.revisions,
    { mode: 'bounded', maximumCount: 50 });
  assert.deepEqual(contracts.find(c => c.key === 'task-board/comments')!.retention,
    { objects: { mode: 'retain' }, revisions: { mode: 'current' } });
  const now = Date.now() + 2 * 24 * 60 * 60 * 1000;
  f.kernel.retention.preview(now);
  f.kernel.retention.collect(now);
  assert.deepEqual((await store.read('task-board/task', first)).value.comments, [comment(1)]);
  assert.deepEqual((await store.read('task-board/task', taskId)).value.comments, [comment(1), comment(2)]);
});

test('conversation metadata snapshots and lost append replies preserve the original comment body', async t => {
  const f = taskBoardFixture(t), created = await f.create(), store = f.first.store, taskId = created.task!.objectId;
  const comments = new TaskComments(store.client, scope => store.preparedMutationId('comments-test:' + scope));
  const initial = await store.read('task-board/task', taskId);
  const pin = await store.write('task-board/task', { ...initial.value, comments: [comment(1)] }, 'append',
    { objectId: taskId, expectedRevision: initial.pin.revision });
  const loaded = await store.read('task-board/task', pin);
  const amended = { ...comment(1), delivery: { objectId: taskId, revision: pin.revision } };
  const updated = await store.write('task-board/task', { ...loaded.value, comments: [amended] }, 'delivery',
    { objectId: taskId, expectedRevision: pin.revision });
  assert.equal((await store.read('task-board/task', pin)).value.comments[0]!.delivery, null);
  assert.deepEqual((await store.read('task-board/task', updated)).value.comments, [amended]);
  await assert.rejects(comments.write({ ...loaded.value, comments: [{ ...amended, body: 'rewritten' }] }, taskId, 'rewrite'),
    { code: 'task_board_comment_mismatch' });
  const next = { ...loaded.value, comments: [amended, comment(2)] };
  let loseReply = true;
  f.intercept(params => {
    if (loseReply && 'create' in params && params.create.contractKey === 'task-board/comment-node') {
      loseReply = false;
      throw new IvyError('synthetic_response_loss', 'The conversation reply was lost.', 'unknown');
    }
  });
  await assert.rejects(comments.write(next, taskId, 'lost-append'), { code: 'synthetic_response_loss' });
  f.intercept(null);
  const retried = await comments.write(next, taskId, 'lost-append');
  assert.deepEqual((await comments.read(retried as never, taskId)).comments, [amended, comment(2)]);
  assert.deepEqual((await store.read('task-board/task', pin)).value.comments, [comment(1)]);
});

test('storage backfill includes archived tickets and preserves inline historical snapshots', async t => {
  const f = taskBoardFixture(t), created = await f.create(), store = f.first.store;
  const initial = await store.read('task-board/task', created.task!.objectId);
  const old = await store.client.request('objects.write', {
    create: { name: 'Old archived ticket', contractKey: 'task-board/task', parentId: store.rootObjectId, ownerObjectId: null },
    contractVersion: '1.9.0', mutationId: await store.preparedMutationId('old-inline-ticket'), references: {},
    content: { encoding: 'json', value: { ...retainedTaskFixture(initial.value), taskKey: 'TASK-0010', comments: [comment(1)] } as never } });
  const taskId = old.object.id;
  await store.client.request('objects.archive', { objectId: taskId, archived: true, mutationId: await store.preparedMutationId('archive-old-ticket') });
  assert.equal(await store.compactComments(2), true);
  const raw = await store.client.request('objects.read', { objectId: taskId });
  assert.equal(raw.revision.contractVersion, '3.0.0');
  assert.equal(raw.object.effectivelyArchived, true);
  assert.deepEqual((await store.read('task-board/task', taskId, true, true)).value.comments, [comment(1)]);
  assert.deepEqual((await store.read('task-board/task', { objectId: taskId, revision: old.revision.revision }, true, true)).value.comments, [comment(1)]);
});

test('deployment protects legacy Run sources before retention and survives a lost migration reply', async t => {
  const w = await nativeWorkflow(t); await w.running();
  const store = w.f.first.store, client = store.client;
  const template = await store.read('task-board/task', w.state().task.objectId);
  const { completionIssue: _issue, ...legacyTask } = retainedTaskFixture(template.value);
  const created = await client.request('objects.write', { create: { contractKey: 'task-board/task', parentId: store.rootObjectId,
    ownerObjectId: null, name: 'Legacy source ticket' }, contractVersion: '1.9.0', references: {},
    mutationId: await store.preparedMutationId('legacy-source'), content: { encoding: 'json', value: {
      ...legacyTask, workflowState: 'todo', waiting: null, claim: null, lastRun: null, publication: null, comments: [],
    } as never } });
  const source = { objectId: created.object.id, revision: created.revision.revision };
  const sourceRead = await client.request('objects.read', source);
  let current = created;
  for (let i = 0; i < 60; i++) current = await client.request('objects.write', {
    objectId: source.objectId, expectedRevision: current.revision.revision, contractVersion: '1.9.0', references: {},
    mutationId: await store.preparedMutationId('legacy-source-' + i), content: sourceRead.content });
  const runTemplate = await store.read('task-board/run', w.state().run);
  const { executionContext: _execution, ...legacyRun } = runTemplate.value;
  const { nextStepId: _step, ...legacyRequest } = legacyRun.originalRequest;
  const originalRequest = { ...legacyRequest, taskId: source.objectId, expectedRevision: source.revision };
  const legacy = await client.request('objects.write', { create: { contractKey: 'task-board/run', parentId: store.rootObjectId,
    ownerObjectId: null, name: 'Legacy unfinished Run' }, contractVersion: '1.2.0', references: {},
    mutationId: await store.preparedMutationId('legacy-run'), content: { encoding: 'json', value: {
      ...legacyRun, taskId: source.objectId, originalRequest, requestHash: hashJson(originalRequest),
    } as never } });
  const legacyRead = await client.request('objects.read', { objectId: legacy.object.id });
  const at = Date.now() + 2 * 86400000;
  w.f.kernel.store.setMetadata('retention_owner_ready:task-board/task:3.0.0', '');
  const registry = w.f.kernel.registry.getRegistry(w.f.first.owner.serviceNodeId)!;
  const oldRegistry = structuredClone(registry);
  oldRegistry.requiredContracts.find(c => c.key === 'task-board/task')!.writeVersions = ['1.10.0'];
  await client.request('registry.sync', oldRegistry);
  await client.request('service.heartbeat', { ready: true, diagnostics: [] });
  assert.equal(w.f.kernel.retention.status(at).families.find(f => f.contractKey === 'task-board/task')!.waitingForOwner, true,
    'an older ready writer cannot acknowledge the new migration');
  await client.request('registry.sync', registry);
  await client.request('service.heartbeat', { ready: false, diagnostics: [] });
  w.f.kernel.retention.collect(at); w.f.kernel.retention.collect(at);
  assert.equal(w.f.kernel.retention.status(at).families.find(f => f.contractKey === 'task-board/task')!.waitingForOwner, true);
  assert.ok(w.f.kernel.store.get('SELECT 1 FROM revisions WHERE object_id=? AND revision=?', source.objectId, source.revision));
  let lost = false;
  w.f.intercept(params => { if (!lost && 'create' in params && params.create.contractKey === 'task-board/revision-protection') {
    lost = true; throw new IvyError('synthetic_response_loss', 'Migration reply was lost.', 'unknown');
  } });
  await assert.rejects(store.protectRunSources(), { code: 'synthetic_response_loss' });
  w.f.intercept(null);
  await w.f.second.store.protectRunSources();
  const ledgers = await client.request('objects.query', { contractKey: 'task-board/revision-protection' });
  assert.equal(ledgers.items.length, 1);
  assert.equal(ledgers.items[0]!.revision, 1, 'retry must recognize the committed ledger');
  assert.deepEqual((await client.request('objects.read', { objectId: legacy.object.id })).revision, legacy.revision);
  assert.equal((await client.request('objects.read', { objectId: source.objectId })).revision.revision, current.revision.revision);
  await client.request('service.heartbeat', { ready: true, diagnostics: [] });
  const heartbeatChanges = w.f.kernel.store.get('SELECT total_changes() AS n')!['n'];
  await client.request('service.heartbeat', { ready: true, diagnostics: [] });
  assert.equal(w.f.kernel.store.get('SELECT total_changes() AS n')!['n'], heartbeatChanges,
    'readiness acknowledgements must not generate writes on every heartbeat');
  w.f.kernel.retention.collect(at); w.f.kernel.retention.collect(at);
  assert.equal(w.f.kernel.retention.status(at).families.find(f => f.contractKey === 'task-board/task')!.waitingForOwner, false);
  assert.ok(w.f.kernel.store.get('SELECT 1 FROM revisions WHERE object_id=? AND revision=?', source.objectId, source.revision));
  assert.equal(Number(w.f.kernel.store.get('SELECT COUNT(*) AS n FROM revisions WHERE object_id=?', source.objectId)!['n']), 51);
  await client.request('objects.write', { objectId: legacy.object.id, expectedRevision: legacy.revision.revision,
    contractVersion: '1.2.0', references: { 'source-task': source }, content: legacyRead.content,
    mutationId: await store.preparedMutationId('run-adopts-source') });
  await store.protectRunSources();
  const cleared = await client.request('objects.read', { objectId: ledgers.items[0]!.objectId });
  assert.deepEqual(cleared.revision.references, {}, 'the bridge retires when the Run protects its own source');
  w.f.kernel.retention.collect(at);
  assert.deepEqual((await client.request('objects.read', source)).content, sourceRead.content);
});

test('comment migration retries tickets skipped during an accepted publication', async t => {
  const f = taskBoardFixture(t), created = await f.create(), store = f.first.store;
  const initial = await store.read('task-board/task', created.task!.objectId);
  const old = await store.client.request('objects.write', { create: { name: 'Publishing legacy ticket', contractKey: 'task-board/task',
    parentId: store.rootObjectId, ownerObjectId: null }, contractVersion: '1.9.0', references: {},
    mutationId: await store.preparedMutationId('publishing-old'), content: { encoding: 'json', value: {
      ...retainedTaskFixture(initial.value), publication: initial.pin, comments: [comment(1)] } as never } });
  assert.equal(await store.compactComments(100), false);
  const value = (await store.client.request('objects.read', { objectId: old.object.id })).content as { encoding: 'json'; value: Record<string, unknown> };
  await store.client.request('objects.write', { objectId: old.object.id, expectedRevision: old.revision.revision,
    contractVersion: '1.9.0', references: {}, mutationId: await store.preparedMutationId('published-old'),
    content: { encoding: 'json', value: { ...value.value, publication: null } as never } });
  t.mock.timers.enable({ apis: ['Date'], now: Date.now() });
  t.mock.timers.tick(60_001);
  assert.equal(await store.compactComments(100), true);
  assert.equal((await store.client.request('objects.read', { objectId: old.object.id })).revision.contractVersion, '3.0.0');
  assert.deepEqual((await store.read('task-board/task', old.object.id)).value.comments, [comment(1)]);
});
