import test from 'node:test';
import assert from 'node:assert/strict';
import { validateTaskBoard } from '../packages/contracts/src/task-board-validation.js';
import { hashJson } from '../packages/contracts/src/canonical.js';
import type { TaskBoard } from '../packages/contracts/src/generated.js';
import { taskBoardContracts, upgradeRecord } from '../services/task-board/src/runtime/schema.js';
import { fields, content } from './fixtures/task-board.js';

const time = '2026-05-01T12:00:00.000Z';
const actor = { principalId: 'user', serviceNodeId: 'task-board', generation: 1, source: 'user' as const };

test('deployed 2.1 conversation contracts remain immutable', () => {
  assert.deepEqual(Object.fromEntries(taskBoardContracts().filter(contract => contract.version === '2.1.0')
    .map(contract => [contract.key, hashJson(contract)])), {
    'task-board/task': 'sha256:b2822550deaafe64f22d8963043efd47bb40a922d4f97fe1d4e90a450f0ca700',
    'task-board/run': 'sha256:dd3aded43b4398b5f491a8bfd8a1e3f310dabfe8a72be6ced2be24bec1fa06b9',
    'task-board/result': 'sha256:2a4f7c24089bb2ead4e0350d60abb4498a03ca380c6c31a6dff8f200297e4d57',
    'task-board/comments': 'sha256:daacdd4070a171f1556e7bfca8353e0c1fbcd34fbb6c659533dfe4d2f3cc1b97',
  });
});

test('published finding-bearing contracts remain immutable after checks are removed', () => {
  assert.deepEqual(Object.fromEntries(taskBoardContracts().filter(contract => contract.version === '2.0.0')
    .map(contract => [contract.key, hashJson(contract)])), {
    'task-board/task': 'sha256:1bfe05ce9a0f34418b00b0080d0c0131e2f168fa2041ec48f2c002a40c5c6a30',
    'task-board/run': 'sha256:3ca0353a7d44c18a15f4ea4df16e51fabef40834fbced5f73f906d0a745be5d8',
    'task-board/result': 'sha256:1862474ec6fc7161c8a5378abd1bd1374e03278dccdec59afe64c87a5e407a38',
    'task-board/comments': 'sha256:0a8312bb4f8c372bbadb3fe7753e86d9c092c4314e4d68d9c5c1136a098a01a1',
  });
  assert.throws(() => validateTaskBoard('ResultContent', { ...content, checks: [] }));
  assert.throws(() => validateTaskBoard('Handoff', { workflowState: 'review', nextSteps: [], checks: [] }));
  assert.throws(() => validateTaskBoard('DeferRequest', { action: 'defer', taskId: 'task', operationId: 'defer',
    expectedRevision: 1, nextSteps: [], checks: [] }));
});

test('findings in older handoffs survive conversion to next steps', () => {
  const findings = [{ key: 'approval', status: 'pending', detail: 'Still awaiting approval.', evidence: [] }];
  const originalComment = { commentId: 'prior-handoff', sequence: 1, author: actor, authorKind: 'agent', body: 'Original handoff.',
    purpose: 'handoff', handoff: { workflowState: 'waiting', nextReviewAt: '2099-01-01T00:00:00.000Z', checks: findings },
    requests: [], responses: [], attachments: [], run: null, turnId: null, replyTo: null, delivery: null,
    operationId: 'prior-handoff', createdAt: time };
  const task = { schemaVersion: 1, taskKey: 'TASK-0001', fields, workflowState: 'waiting', waiting: null, publication: null,
    primaryResourceRef: null, claim: null, attemptCount: 0, lastRun: null, lastHistory: null, latestResult: null,
    acceptedReview: null, comments: [originalComment], commentDeliveries: [], agentCommentCount: 1, workRevision: 0,
    attachments: [], createdAt: time, updatedAt: time };
  const upgraded = upgradeRecord('task-board/task', task as unknown as TaskBoard.Task);
  assert.equal(upgraded.comments[0]!.body, originalComment.body);
  assert.equal('checks' in upgraded.comments[0]!.handoff!, false);
  assert.equal(upgraded.comments.length, 2);
  const body = upgraded.comments[1]!.body;
  assert.deepEqual(JSON.parse(body.slice(body.indexOf('json\n') + 5, body.lastIndexOf('\n'))), findings);
  assert.doesNotThrow(() => validateTaskBoard('Task', upgraded));
});

test('published Task contract definitions stay stable when tool descriptions change', () => {
  const contracts = taskBoardContracts().filter(value => value.key === 'task-board/task' && !['1.10.0', '1.11.0', '1.12.0', '2.0.0', '2.1.0', '2.2.0', '2.3.0'].includes(value.version));
  assert.deepEqual(Object.fromEntries(contracts.map(value => [value.version, hashJson(value)])), {
    '1.9.0': 'sha256:d77cdc4472e267298847aed6fa7f207d546fbe96afadc1a379c5a886961164ff',
    '1.8.0': 'sha256:00a22238b737fc5e1f178c5dd239b88aaa2467220c103bf7ff3094772cc70db7',
    '1.7.0': 'sha256:3292027f5a0bfcc962c9c91fce6363ec046c3b0c910912959900c77af39dfa3b',
    '1.6.0': 'sha256:7c4a5106d614116242641156bb4f15920ff509d255a47119c033363470a6da5d',
    '1.0.0': 'sha256:feb604f282eee8a074b2f0487dc5b11b133a2363d6039b8321fa4e07cd0639d6',
    '1.1.0': 'sha256:e68a335f0f888c3b703556d32f22b7d742addbec1f3177e54962be3b1521d602',
    '1.2.0': 'sha256:d9c75e80b11c0117cea38a13a8f6c98e22c5dd443bd69f91e3093151233405ef',
    '1.4.0': 'sha256:df612702ff849a4dcab3b5dbb5717ef97c167701c719306fb11880a2e5e95b07',
    '1.3.0': 'sha256:d86bbe221890c9fc9e2e4a4e45cbe208f69fb7a6526292de05087c602af335ac',
    '1.5.0': 'sha256:826744c94f3b65686d3b34ed149f83adc5f6a4ecb8f1be317e2b51abf00cb7f9',
  });
  assert.deepEqual(Object.fromEntries(taskBoardContracts().filter(value => value.key === 'task-board/configuration').map(value => [value.version, hashJson(value)])), {
    '1.2.0': 'sha256:d5ec19df923164e231d76e272b366ffb892890b61445c5eedc3c7f609646d107',
    '1.0.0': 'sha256:72a13300211dfc025adc5849cd3f1da98d84584bfec7e42868f9864d09ecbbab',
    '1.1.0': 'sha256:d79b949925a2504f033148496f5853715a2dcf70af659d4bc6a789487fbe9613',
  });
  assert.equal(hashJson(taskBoardContracts().find(value => value.key === 'task-board/run' && value.version === '1.0.0')!),
    'sha256:ebf2011875459c74f6d7f07f8ae6a4d5c596e3eba726c2a59f4a2fbd6ea4aa98');
  assert.equal(hashJson(taskBoardContracts().find(value => value.key === 'task-board/delivery' && value.version === '1.0.0')!),
    'sha256:8633eacd16bc5a4c3254c2ff465a7f508277d8d595111b4733c7db1652332a3c');
});

test('published compact tasks, comments and Run contracts remain unchanged by completion recovery', () => {
  const selected = taskBoardContracts().filter(value => value.key === 'task-board/task' && ['1.10.0', '1.11.0'].includes(value.version) ||
    value.key === 'task-board/run' && value.version === '1.2.0' || value.key === 'task-board/comments' && value.version === '1.0.0');
  assert.deepEqual(Object.fromEntries(selected.map(value => [value.key + '@' + value.version, hashJson(value)])), {
    'task-board/task@1.11.0': 'sha256:6fc7e6ff9cb78cd56fbc6d080d4675d8136d13db710bb3aa4ada1ae06c291ae7',
    'task-board/task@1.10.0': 'sha256:b6a0ccb154303242d8e2bb204587733b9b4e5f10d188aa360e98a152311324e8',
    'task-board/comments@1.0.0': 'sha256:e111eb69561fe00c67c6f5b669ae16a403a7f19613dfbc4aa14c30ecbdd2cfd8',
    'task-board/run@1.2.0': 'sha256:bebd82533ebffc0ee65d480c8e62ecb8c1f4205b946877cbdf0c6f03776b412d',
  });
});

test('older capability requirements read as automatic placement with a required capability', () => {
  const task = { schemaVersion: 1 as const, taskKey: 'TASK-0001', fields: { ...fields, executionRequirement: { kind: 'capability', capabilityKey: 'video-editing' } as unknown as TaskBoard.ExecutionRequirement },
    workflowState: 'todo' as const, waiting: null, publication: null, primaryResourceRef: null, claim: null, attemptCount: 0, lastRun: null, lastHistory: null, latestResult: null,
    acceptedReview: null, comments: [], commentDeliveries: [], agentCommentCount: 0, workRevision: 0, attachments: [], createdAt: time, updatedAt: time };
  assert.throws(() => validateTaskBoard('Task', task));
  const upgraded = upgradeRecord('task-board/task', task as unknown as TaskBoard.Task);
  assert.deepEqual(upgraded.fields.executionRequirement, { kind: 'automatic' });
  assert.deepEqual(upgraded.fields.requiredCapabilities, ['video-editing']);
  assert.doesNotThrow(() => validateTaskBoard('Task', upgraded));
  const configuration = upgradeRecord('task-board/configuration', { schemaVersion: 1, defaults: { executionRequirement: { kind: 'capability', capabilityKey: 'gpu' } as unknown as TaskBoard.ExecutionRequirement, nativeOptions: { model: null, reasoningEffort: null, serviceTier: null } } });
  assert.deepEqual(configuration.defaults.executionRequirement, { kind: 'automatic' });
});

test('new workflow contracts accept Task keys, states, comments and workspace requirements', () => {
  const task = { schemaVersion: 1 as const, taskKey: 'TASK-0001', fields, workflowState: 'backlog' as const, nextSteps: [], runtimeWait: null, publication: null, primaryResourceRef: null, claim: null,
    attemptCount: 0, lastRun: null, lastHistory: null, latestResult: null, acceptedReview: null, comments: [], commentDeliveries: [], agentCommentCount: 0, workRevision: 0, attachments: [], createdAt: time, updatedAt: time };
  assert.doesNotThrow(() => validateTaskBoard('Task', task));
  assert.throws(() => validateTaskBoard('Task', { ...task, taskKey: 'task-1' }));
  assert.throws(() => validateTaskBoard('Task', { ...task, workflowState: 'ready' }));
  assert.doesNotThrow(() => validateTaskBoard('CreateRequest', { action: 'create', operationId: 'create-one', fields }));
  assert.doesNotThrow(() => validateTaskBoard('TransitionRequest', { action: 'transition', operationId: 'move-one', taskId: 'task-one', expectedRevision: 1, workflowState: 'todo', detail: null }));
});

test('execution requests require exact target, workspace and queued comments', () => {
  const workspace = { hostId: 'host', serviceNodeId: 'agent', canonicalCwd: '/work/TASK-0001', taskRoot: '/work', bootstrapPath: '/work/TASK-0001', intendedPath: '/work/TASK-0001', sourcePath: null, project: null, useWorktree: false, repository: null };
  const request = { action: 'start', operationId: 'start-one', taskId: 'task-one', expectedRevision: 2, target: { hostId: 'host', serviceNodeId: 'agent' }, intent: { objectId: 'plan', revision: 1 }, workspace, commentIds: [] };
  assert.doesNotThrow(() => validateTaskBoard('StartRequest', request));
  const { workspace: _workspace, ...missing } = request;
  assert.throws(() => validateTaskBoard('StartRequest', missing));
});

test('results distinguish repository provenance from non-Git work', () => {
  assert.doesNotThrow(() => validateTaskBoard('ResultContent', content));
  assert.doesNotThrow(() => validateTaskBoard('ResultContent', { ...content, repositoryResult: { hostId: 'host', serviceNodeId: 'agent', project: { serviceNodeId: 'agent', namespace: 'codex', kind: 'project', nativeId: 'project' }, repositoryName: 'repo', branch: 'main', commit: 'abc', originName: 'origin', originUrl: 'https://example.test/repo.git', originState: 'unknown', remoteRef: null, observedAt: time, limitation: 'Remote reachability not checked.' } }));
  assert.throws(() => validateTaskBoard('ResultContent', { ...content, repositoryResult: undefined }));
});

test('comments and attachments use bounded typed metadata', () => {
  const attachment = { attachmentId: 'attachment-one', object: { objectId: 'blob', revision: 1 }, filename: 'report.pdf', mediaType: 'application/pdf', byteLength: 3, contentHash: 'sha256:' + '0'.repeat(64), uploader: actor, createdAt: time };
  assert.doesNotThrow(() => validateTaskBoard('TaskAttachment', attachment));
  assert.doesNotThrow(() => validateTaskBoard('Comment', { commentId: 'comment-one', sequence: 1, author: actor, authorKind: 'user', body: 'Review this.', requests: [], responses: [], delivery: null, attachments: [attachment], run: null, turnId: null, replyTo: null, createdAt: time, operationId: 'comment-op' }));
  assert.doesNotThrow(() => validateTaskBoard('CommentActionRequest', { action: 'comment', operationId: 'approval-op', taskId: 'task-one', expectedRevision: 1,
    commentId: 'approval-comment', body: 'Please approve.', requests: [{ requestId: 'approval-one', type: 'approval', title: 'Deploy', detail: 'Approve deployment.' }], responses: [], attachments: [], replyTo: null }));
});

test('TaskBoard publishes explicit retention for every contract including attachment bytes', () => {
  const contracts = taskBoardContracts();
  assert.ok(contracts.length >= 8);
  assert.ok(contracts.every(contract => contract.retention));
  const attachment = contracts.find(contract => contract.key === 'task-board/attachment');
  assert.deepEqual(attachment?.retention, { objects: { mode: 'owned' }, revisions: { mode: 'current' } });
});
