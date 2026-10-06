import test from 'node:test';
import assert from 'node:assert/strict';
import { checkedNativeContract } from '../packages/contracts/src/checked-native-contract.js';
import { NativeContract } from '../packages/contracts/src/native-contract.js';
import { digest } from '../packages/contracts/src/canonical.js';
import { nativeThreadState, nativeTurnItems } from '../packages/sdk/src/native-observations.js';

for (const version of ['0.154.0']) test('native observation interpretation follows the selected ' + version + ' schema', () => {
  const { contract } = checkedNativeContract(version), thread = { cliVersion: version, createdAt: 1788690000, cwd: '/fixture', ephemeral: false,
    id: 'original-thread', modelProvider: 'openai', preview: '', projectId: null, sessionId: 'session', source: 'appServer',
    status: { type: 'idle' }, turns: [], updatedAt: 1788690000 };
  const current = { ...thread, canAcceptDirectInput: true };
  assert.equal(nativeThreadState(contract, { thread: current }).canStartTurn, true);
  assert.equal(nativeThreadState(contract, { thread }).canStartTurn, false);
  assert.equal(nativeThreadState(contract, { thread: { ...current, canAcceptDirectInput: false } }).canStartTurn, false);
  for (const state of ['active', 'notLoaded']) assert.equal(nativeThreadState(contract,
    { thread: { ...current, status: state === 'active' ? { type: state, activeFlags: [] } : { type: state } } }).canStartTurn, false);
  assert.equal(nativeThreadState(contract, { thread: { ...current, status: { type: 'invented' } } }).canStartTurn, false);
  const item = { id: 'answer', type: 'agentMessage', text: 'Complete retained answer.' };
  const page = { data: [{ turnId: 'original-turn', item }], nextCursor: null };
  assert.deepEqual(nativeTurnItems(contract, page, 'original-turn'), [item]);
  assert.throws(() => nativeTurnItems(contract, { ...page, data: [{ turnId: 'other-turn', item }] }, 'original-turn'));
  assert.throws(() => nativeTurnItems(contract, { data: [{ fabricated: true }], nextCursor: null }, 'original-turn'));
  const copy = nativeTurnItems(contract, page, 'original-turn'); copy[0]!.text = 'changed'; assert.equal(item.text, 'Complete retained answer.');
});

test('new provider version with the same declared shape needs no service version branch', () => {
  const catalog = structuredClone(checkedNativeContract('0.154.0').catalog); catalog.version = '9.999.0'; catalog.sourceHash = digest('synthetic-new-version');
  const contract = new NativeContract(catalog), item = { id: 'answer', type: 'agentMessage', text: 'Synthetic shape equivalence only.' };
  assert.deepEqual(nativeTurnItems(contract, { data: [{ turnId: 'turn', item }], nextCursor: null }, 'turn'), [item]);
  assert.throws(() => nativeTurnItems(contract, { data: [item], nextCursor: null }, 'turn'));
});
