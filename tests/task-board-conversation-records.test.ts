import test from 'node:test';
import assert from 'node:assert/strict';
import type { TaskBoard } from '../packages/contracts/src/generated.js';
import { taskBoardFixture } from './fixtures/task-board.js';
import { nativeWorkflow } from './fixtures/task-board-native.js';
import { CommentReader, commentNodeKey } from '../packages/sdk/src/task-comments.js';
import type { CommentTree } from '../packages/sdk/src/task-comments.js';
import { IvyError, canonical, hashJson } from '../packages/sdk/src/node.js';
import { routeTaskQuestions } from '../services/task-board/src/runtime/question-routing.js';
import { openQuestions } from '../services/task-board/src/runtime/conversation.js';
import { taskCard, taskViewPage } from '../services/task-board/src/runtime/task-card.js';
import { assessSteps, stepCondition } from '../services/task-board/src/runtime/next-steps.js';
import { executionCondition } from '../services/task-board/src/runtime/condition.js';
import { nextExecutionMode } from '../services/task-board/src/runtime/execution-mode.js';
import { TaskBoardStore } from '../services/task-board/src/runtime/store.js';
import { setImmediate } from 'node:timers/promises';

const comment = (sequence: number): TaskBoard.Comment => ({ commentId: 'comment-' + sequence, sequence,
  author: { principalId: 'user', serviceNodeId: 'task-board', generation: 1, source: 'user' }, authorKind: 'user',
  body: 'Conversation body '.repeat(1024), requests: [], responses: [], delivery: null, attachments: [], run: null,
  turnId: null, replyTo: null, createdAt: '2026-05-01T12:00:00.000Z', operationId: 'comment-' + sequence });

test('conversation exceeds the old object limit, reads bounded pages and survives a lost publication reply', async t => {
  const f = taskBoardFixture(t), created = await f.create(), store = f.first.store;
  const original = await store.read('task-board/task', created.task!.objectId);
  const comments = Array.from({ length: 100 }, (_, index) => comment(index + 1));
  const saved = await store.write('task-board/task', { ...original.value, comments }, 'large-conversation',
    { objectId: original.pin.objectId, expectedRevision: original.pin.revision });
  const raw = await store.client.request('objects.read', saved);
  assert.ok(raw.revision.byteLength < 5000);
  const tree = (raw.content.value as unknown as { commentStore: CommentTree }).commentStore;
  let reads = 0;
  const reader = new CommentReader({ request: async (...args: any[]) => {
    reads++; return (store.client.request as any)(...args);
  } } as never);
  const page = await reader.range(tree, saved.objectId, 90, 100);
  assert.deepEqual(page.map(entry => entry.comment.commentId), comments.slice(90).map(comment => comment.commentId));
  assert.ok(reads <= 12, 'a ten-comment page reads only ten records and its two index pages');
  const complete = await store.read('task-board/task', saved);
  assert.ok(Buffer.byteLength(canonical(complete.value)) > 1024 * 1024);
  assert.deepEqual(complete.value.comments, comments);
  const receipt: TaskBoard.CommentDelivery = { commentId: 'comment-1', state: 'delivered',
    operationId: 'delivery', updatedAt: original.value.createdAt, code: null };
  let lost = false;
  f.intercept(params => {
    if (!lost && 'objectId' in params && params.objectId === saved.objectId) {
      lost = true; throw new IvyError('synthetic_response_loss', 'Lost header reply.', 'unknown');
    }
  });
  const next = { ...complete.value, commentDeliveries: [receipt], comments: [...comments, comment(101)] };
  await assert.rejects(store.write('task-board/task', next, 'lost-header',
    { objectId: saved.objectId, expectedRevision: saved.revision }), { code: 'synthetic_response_loss' });
  f.intercept(null);
  const retried = await store.write('task-board/task', next, 'lost-header',
    { objectId: saved.objectId, expectedRevision: saved.revision });
  assert.equal((await store.read('task-board/task', retried)).value.comments.length, 101);
  assert.deepEqual((await store.read('task-board/task', saved)).value.commentDeliveries, []);
  assert.deepEqual((await store.read('task-board/task', retried)).value.commentDeliveries, [receipt]);
  const view = await f.first.task({ task: saved.objectId }, 'user');
  assert.equal(taskViewPage(view, { task: saved.objectId }).task.comments.length, 50);
  assert.deepEqual(taskViewPage(view, { task: saved.objectId, comments: { offset: 0, limit: 10 } }).commentPage,
    { total: 101, offset: 0, nextOffset: 10 });
});

test('many foreign conversation objects do not increase work for a ticket record lookup', async t => {
  const f = taskBoardFixture(t), created = await f.create(), store = f.first.store;
  const initial = await store.read('task-board/task', created.task!.objectId);
  await store.write('task-board/task', { ...initial.value, comments: [comment(1)] }, 'seed',
    { objectId: initial.pin.objectId, expectedRevision: initial.pin.revision });
  const selected = (await store.client.request('objects.query', { contractKey: commentNodeKey, limit: 1 })).items[0]!;
  const object = await store.client.request('objects.read', { objectId: selected.objectId });
  const db = f.kernel.store.db;
  db.exec('BEGIN');
  try {
    const objects = db.prepare(`INSERT INTO objects(id,parent_id,owner_object_id,name,path,position,icon,depth,contract_key,current_revision,archived_at,effective_archive,created_at,updated_at)
      SELECT ?,parent_id,owner_object_id,?,?,position,icon,depth,contract_key,current_revision,archived_at,effective_archive,created_at,updated_at FROM objects WHERE id=?`);
    const revisions = db.prepare(`INSERT INTO revisions SELECT ?,revision,contract_key,contract_version,encoding,content,content_hash,byte_length,created_at,references_json FROM revisions WHERE object_id=?`);
    for (let i = 0; i < 10000; i++) {
      const id = 'foreign-' + i;
      objects.run(id, id, '/foreign/' + i, selected.objectId); revisions.run(id, selected.objectId);
    }
    db.exec('COMMIT');
  } catch (error) { db.exec('ROLLBACK'); throw error; }
  let visited = 0;
  db.function('ivy_budget', () => { visited++; return 1; });
  const found = await store.client.request('objects.query', { contractKey: commentNodeKey, where: { op: 'and', args: [
    { field: 'object.parentId', op: 'eq', value: initial.pin.objectId },
    { field: 'object.name', op: 'eq', value: object.object.name },
  ] }, limit: 1 });
  assert.equal(found.items[0]!.objectId, selected.objectId);
  assert.ok(visited <= 2, 'indexed lookup must not scan the other 10,000 records; visited=' + visited);
});

test('concurrent conversation reads share a connection budget instead of multiplying page concurrency', async t => {
  const f = taskBoardFixture(t), created = await f.create(), source = f.first.store;
  const original = await source.read('task-board/task', created.task!.objectId);
  const comments = Array.from({ length: 80 }, (_, index) => ({ ...comment(index + 1), body: 'Small entry.' }));
  const saved = await source.write('task-board/task', { ...original.value, comments }, 'concurrent-history',
    { objectId: original.pin.objectId, expectedRevision: original.pin.revision });
  const raw = await source.client.request('objects.read', saved);
  const tree = (raw.content.value as unknown as { commentStore: CommentTree }).commentStore;
  let active = 0, peak = 0, reads = 0;
  const store = new TaskBoardStore({ request: async (method, params, options) => {
    active++; reads++; peak = Math.max(peak, active);
    try {
      assert.ok(active < 64, 'leave capacity for connection management');
      await setImmediate();
      return await source.client.request(method, params, options);
    } finally { active--; }
  } }, null);
  t.after(() => store.close());
  const reader = new CommentReader(store.client);
  const pages = await Promise.all(Array.from({ length: 20 }, () => reader.range(tree, saved.objectId)));
  assert.ok(reads > 64, 'exercise a cold multi-ticket-size read burst');
  assert.ok(peak <= 48);
  assert.equal(active, 0);
  for (const page of pages) assert.deepEqual(page.map(entry => entry.comment.commentId), comments.map(entry => entry.commentId));
});

test('dependency status checks do not load the foreign ticket conversation', async t => {
  const f = taskBoardFixture(t), created = await f.create(), store = f.first.store;
  const own = await store.read('task-board/task', created.task!.objectId);
  const other = await f.create();
  const foreign = await store.read('task-board/task', other.task!.objectId);
  const comments = Array.from({ length: 20 }, (_, index) => comment(index + 1));
  await store.write('task-board/task', { ...foreign.value, comments }, 'foreign-conversation',
    { objectId: foreign.pin.objectId, expectedRevision: foreign.pin.revision });
  let conversationReads = 0, headerReads = 0;
  const checking = new TaskBoardStore({ request: async (method, params, options) => {
    const result = await store.client.request(method, params, options);
    if (method === 'objects.read') {
      const object = (result as { object: { id: string; contractKey: string } }).object;
      if (object.contractKey === commentNodeKey) conversationReads++;
      if (object.id === foreign.pin.objectId) headerReads++;
    }
    return result;
  } }, null);
  t.after(() => checking.close());
  const assessments = await assessSteps(checking, { ...own.value, nextSteps: [
    { id: 'after-peer', scope: 'Review the idle peer.', requires: [{ kind: 'task', taskId: foreign.pin.objectId, until: 'idle' }] },
  ] }, new Date().toISOString());
  assert.equal(assessments[0]?.ready, true);
  assert.equal(headerReads, 1);
  assert.equal(conversationReads, 0);
});

test('resuming a compact prepared publication preserves the complete conversation and its exact payload hash', async t => {
  const f = taskBoardFixture(t), created = await f.create(), store = f.first.store;
  const initial = await store.read('task-board/task', created.task!.objectId);
  const comments = [comment(1), comment(2), comment(3)];
  const saved = await store.write('task-board/task', { ...initial.value, comments }, 'seed-recovery',
    { objectId: initial.pin.objectId, expectedRevision: initial.pin.revision });
  const request: TaskBoard.TransitionRequest = { action: 'transition', operationId: 'resume-conversation',
    taskId: saved.objectId, expectedRevision: saved.revision, workflowState: 'todo', detail: 'Resume retained work.' };
  let lost = false;
  f.intercept(params => {
    if (!lost && 'objectId' in params && params.objectId === saved.objectId) {
      lost = true; throw new IvyError('connection_lost', 'Lost reservation reply.', 'unknown');
    }
  });
  await assert.rejects(f.invoke(request), { code: 'connection_lost' });
  f.intercept(null);
  const result = await f.invoke(request, 'user', f.second);
  const recovered = await f.second.store.read('task-board/task', result.task!);
  assert.deepEqual(recovered.value.comments, comments);
  assert.equal(recovered.value.workflowState, 'todo');
  assert.equal(recovered.value.publication, null);
  assert.deepEqual((await store.read('task-board/task', saved)).value.comments, comments);
});

test('task questions route once, stay out of user badges and are answered by the addressed task', async t => {
  const w = await nativeWorkflow(t); await w.running();
  const f = w.f, store = f.first.store, source = await store.read('task-board/task', w.state().task.objectId);
  const target = await f.create({ control: 'agent' });
  const asked = await f.worker({ action: 'comment', operationId: 'ask-target', taskId: source.pin.objectId, expectedRevision: source.pin.revision,
    commentId: 'internal-question', authorKind: 'agent', purpose: 'question', recipient: { kind: 'user' },
    body: 'Which contract should I apply?', requests: [], responses: [], attachments: [], replyTo: null, agentDelivery: 'none' });
  await f.worker({ action: 'retargetQuestion', operationId: 'correct-recipient', taskId: source.pin.objectId,
    expectedRevision: asked.task!.revision, questionId: 'internal-question', recipient: { kind: 'task', taskId: target.task!.objectId } });
  await routeTaskQuestions(f.first, source.pin.objectId);
  await routeTaskQuestions(f.first, source.pin.objectId);
  const receiver = await store.read('task-board/task', target.task!.objectId);
  assert.equal(receiver.value.comments.filter(comment => comment.questionSource).length, 1);
  assert.equal(receiver.value.commentDeliveries.filter(delivery => delivery.state === 'queued').length, 1);
  const view = await f.first.task({ task: source.pin.objectId }, 'user');
  assert.equal(taskCard(view).task.openQuestionCount, 0);
  assert.equal(taskCard(view).directCommentCount, 0);
  assert.equal(taskViewPage(view, { task: source.pin.objectId }).openQuestions!.length, 0);
  const gated = { ...view.task, nextSteps: [{ id: 'answer', scope: 'Use the contract.',
    requires: [{ kind: 'question' as const, questionId: 'internal-question' }] }] };
  assert.equal(stepCondition(await assessSteps(store, gated, new Date().toISOString())).state, 'blocked_dependency');
  const answered = { ...view.task, comments: [...view.task.comments, { ...comment(2), authorKind: 'agent' as const,
    sourceTaskId: target.task!.objectId, replyTo: 'internal-question' }] };
  assert.equal(openQuestions(answered).length, 0);
  const wrong = { ...answered, comments: [...view.task.comments, { ...comment(2), authorKind: 'agent' as const,
    sourceTaskId: 'unrelated-task', replyTo: 'internal-question' }] };
  assert.equal(openQuestions(wrong).length, 1);
});

test('a permanent completion error is a failure and exhausted recovery is never a user question', () => {
  const facts = { workflowState: 'waiting' as const, control: 'agent' as const, claim: null,
    publication: false, runtimeWait: null, executionRequirement: null, completionIssue: 'content_too_large: handoff refused' };
  assert.equal(executionCondition(facts).state, 'failed');
  assert.equal(executionCondition({ ...facts, recovery: { attempts: 2, retryAt: null } }).state, 'failed');
});

test('a retained storage refusal survives completion instead of triggering missing-handoff recovery', async t => {
  const w = await nativeWorkflow(t); await w.running();
  const engine = w.f.first, task = await engine.store.read('task-board/task', w.state().task.objectId);
  const { priority: _priority, dueAt: _due, userContact: _contact, ...fields } = task.value.fields;
  const request: TaskBoard.CommentActionRequest = { action: 'comment', operationId: 'refused-handoff',
    taskId: task.pin.objectId, expectedRevision: task.pin.revision, commentId: 'refused',
    purpose: 'handoff', authorKind: 'agent', body: 'Completed work.', handoff: { workflowState: 'review', nextSteps: [] },
    requests: [], responses: [], attachments: [], replyTo: null };
  const operation = await engine.operations.begin(request, { ...engine.owner, principalId: 'worker', source: 'worker' },
    async () => ({ run: w.state().run, work: { claimOperationId: task.value.claim!.operationId,
      workRevision: task.value.workRevision, primaryResourceRef: task.value.primaryResourceRef,
      contentHash: hashJson({ fields, parentTaskId: task.value.parentTaskId ?? null }) } }));
  await engine.operations.failure(operation.pin.objectId, new IvyError('content_too_large', 'Conversation exceeded storage capacity.', 'not_executed'), false);
  await w.update((await w.snapshot()).change);
  const failed = (await engine.store.read('task-board/task', task.pin.objectId)).value;
  assert.equal(failed.completionIssue?.code, 'content_too_large');
  assert.equal(failed.completionIssue?.operationId, 'refused-handoff');
  assert.match(failed.completionIssue!.detail, /Conversation exceeded storage capacity/);
  assert.equal(engine.condition(failed).state, 'failed');
  assert.equal(nextExecutionMode(failed, new Date().toISOString()), null);
});
