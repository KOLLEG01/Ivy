import test from 'node:test';
import assert from 'node:assert/strict';
import { IvyError } from '../dist/packages/sdk/src/node.js';
import { teamsFixture } from './secretary-teams-fixture.mjs';

test('retained native Teams plans close an absent old-epoch remainder only after original thread cleanup', { timeout: 90000 }, async t => {
  const n = await teamsFixture(t);
  n.state.afterNative = operation => { if (operation.method === 'thread/start') { n.state.epoch = 'replacement-epoch'; n.state.afterNative = null; } };
  assert.equal(await n.settle(), 'gap'); const head = await n.source.head(), proof = await n.source.evidence(head.value.lastPage);
  assert.equal(head.value.pending, null); assert.equal(head.value.lastCompleteAt, null); assert.equal(proof.value.gap, 'microsoft_epoch_changed');
  assert.deepEqual(n.state.dispatches.map(call => call.method), ['account/read', 'thread/start', 'thread/unsubscribe']);
  assert.equal((await n.f.items()).length, 0); await n.restart(); assert.equal((await n.source.head()).value.pending, null);
});

test('Teams resumes original message-page evidence captures checkpoint and final head after every lost commit reply', { timeout: 240000 }, async t => {
  const boundaries = [ ['secretary/item', 1] ];
  for (const [key, revision] of boundaries) {
    const n = await teamsFixture(t); assert.equal(await n.settle(), 'page'); let hit = false;
    n.state.after = async (method, _args, value) => { if (!hit && method === 'objects.write' && value.object.contractKey === key && value.revision.revision === revision) { hit = true; n.state.cut = true; throw new IvyError('outcome_unknown', 'Lost committed response.', 'unknown'); } };
    await assert.rejects(n.settle(), undefined, key + ':' + revision); assert.ok(hit, key + ':' + revision); n.state.after = null; await n.restart();
    assert.ok(['complete', 'idle'].includes(await n.settle())); assert.equal((await n.f.items()).length, 1); assert.equal(n.state.dispatches.length, 14); assert.equal(new Set(n.state.dispatches.map(x => x.operationId)).size, 14);
    assert.equal((await n.f.engine.source('teams')).pin.revision, 3); assert.ok((await n.source.head()).value.lastCompleteAt);
  }
});
