import test from 'node:test';
import assert from 'node:assert/strict';
import { canonical, digest } from '../dist/packages/sdk/src/node.js';
import { triageFixture } from './secretary-triage-fixture.mjs';
import { triageName } from '../dist/services/secretary/src/triage-store.js';

test('large original native JSON is retained in the local journal across restart', { timeout: 60000 }, async t => {
  const f = await triageFixture(t), identity = [{ objectId: 'original-plan', revision: 1 }, 'turn'];
  const value = { method: 'turn/start', params: { input: [{ type: 'text', text: 'Original ä\n'.repeat(140000) }] } };
  const bytes = Buffer.from(canonical(value)); assert.ok(bytes.length > 1024 * 1024);
  const store = f.worker.native.store, pin = await store.saveNativeJson('request', identity, value);
  const raw = f.f.engine.store.technicalBlob('secretary/triage-native-request', triageName('native-request', [identity, digest(bytes)]), pin);
  assert.deepEqual(raw.bytes, bytes);
  await f.restart(); f.state.offline = true;
  assert.deepEqual(await f.worker.native.store.nativeJson('request', identity, pin), value);
  assert.deepEqual(await f.worker.native.store.saveNativeJson('request', identity, value), pin);
  assert.equal(f.state.dispatches.length, 0);
  await assert.rejects(store.saveNativeJson('request', identity, { text: 'x'.repeat(8 * 1024 * 1024) }), { code: 'content_too_large' });
});

test('pinned native storage retains exact local bytes', { timeout: 60000 }, async t => {
  const f = await triageFixture(t), store = f.worker.native.store, identity = ['original-plan', 'turn'], value = { operationId: f.f.operationId(), original: true };
  const pin = await store.saveNativeJson('operation', identity, value);
  assert.deepEqual(await store.nativeJson('operation', ['other-plan', 'turn'], pin), value);
  assert.deepEqual(await store.saveNativeJson('operation', identity, value), pin);
  await assert.rejects(store.nativeJson('operation', identity, { objectId: 'missing', revision: 1 }), { code: 'secretary_triage_evidence_invalid' });
});

test('a lost native reply resumes the original local call without another thread or turn', { timeout: 90000 }, async t => {
  const f = await triageFixture(t), item = await f.capture(); f.state.lostReply = true;
  assert.equal(await f.worker.step(item), 'assessed'); assert.equal(f.state.dispatches.length, 2);
  f.state.lostReply = false; await f.restart(); assert.equal(await f.worker.step(item), 'assessed');
  assert.deepEqual(f.state.dispatches.map(call => call.method), ['thread/start', 'turn/start']);
  assert.equal(f.f.engine.store.technicalList('secretary/triage-call').length, 2);
  await f.restart(); f.state.offline = true; assert.equal(await f.worker.step(item), 'assessed'); assert.equal(f.state.dispatches.length, 2);
});
