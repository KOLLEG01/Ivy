import test from 'node:test';
import assert from 'node:assert/strict';
import { validateAgent } from '../dist/packages/sdk/src/node.js';
import { outlookFixture } from './secretary-outlook-fixture.mjs';
import { teamsFixture } from './secretary-teams-fixture.mjs';

for (const [kind, fixture] of [['outlook', outlookFixture], ['teams', teamsFixture]]) {
  test(`${kind} retains latest pending progress through a lost write reply and restart without redispatch`, { timeout: 90000 }, async t => {
    const n = await fixture(t); n.state.pendingMethod = 'account/read';
    assert.equal(await n.source.step(), 'pending');
    const accepted = structuredClone(n.state.dispatches[0]), operationId = accepted.operationId;
    const unknown = { ...accepted, phase: 'outcome_unknown', epoch: n.state.epoch, requestId: 70, code: 'native_epoch_lost' };
    validateAgent('Operation', unknown); n.state.operations.set(operationId, unknown);
    assert.equal(await n.source.step(), 'pending'); await n.restart();
    const [row] = n.f.engine.store.technicalList(`secretary/${kind}-call`);
    let call = await n.source.store.read(`secretary/${kind}-call`, row.pin);
    assert.equal(call.pin.revision, 3);
    assert.deepEqual(await n.source.store.operation(call.value.plan, 0, call.value.seen), unknown);
    // An identical observation adds no revision; the original intent and operation remain fixed.
    assert.equal(await n.source.step(), 'pending');
    assert.equal((await n.source.store.read(`secretary/${kind}-call`, call.pin.objectId)).pin.revision, 3);
    for (const bad of [accepted, { ...unknown, epoch: 'another-epoch' }, { ...unknown, requestId: 71 }]) {
      n.state.operations.set(operationId, bad); await n.restart();
      await assert.rejects(n.source.step(), { code: 'native_evidence_regressed' });
      assert.equal(n.state.dispatches.length, 1);
    }
    n.state.operations.set(operationId, unknown); n.state.absent = true; await n.restart();
    await assert.rejects(n.source.step(), { code: 'microsoft_native_absence_conflict' });
    n.state.absent = false; n.state.pendingMethod = null;
    n.state.operations.set(operationId, { ...unknown, phase: 'succeeded', reply: { result: n.state.results.get(operationId) }, code: null });
    assert.equal(await n.settle(), kind === 'teams' ? 'page' : 'complete');
    if (kind === 'teams') assert.equal(await n.settle(), 'complete');
    const dispatches = n.state.dispatches.length; assert.equal(dispatches, kind === 'teams' ? 14 : 7);
    call = await n.source.store.read(`secretary/${kind}-call`, call.pin.objectId);
    assert.equal(call.pin.revision, 4);
    assert.deepEqual(await n.source.store.operation(call.value.plan, 0, call.value.seen), unknown);
    n.state.offline = true; await n.restart(); assert.equal(await n.source.step(), 'idle');
    assert.equal(n.state.dispatches.length, dispatches);
  });
}
