import test from 'node:test';
import assert from 'node:assert/strict';
import { deriveOperationId, hashJson } from '../dist/packages/sdk/src/node.js';
import { mediaNativeFixture as fixture } from './secretary-media-native-fixture.mjs';

test('media epoch closure retains original reads, cleans the original thread and acquires no replacement payload', { timeout: 90000 }, async t => {
  const f = await fixture(t), count = f.n.state.dispatches.length;
  f.n.state.afterNative = operation => { if (operation.method === 'thread/start') { f.n.state.epoch = 'replacement-media-epoch'; f.n.state.afterNative = null; } };
  const coverage = await f.settle(); assert.equal(coverage.gap, 'media_native_epoch_changed'); assert.equal(f.gets, 0);
  assert.deepEqual(f.n.state.dispatches.slice(count).map(call => call.method), ['account/read', 'thread/start', 'thread/unsubscribe']);
  assert.equal(f.n.state.dispatches.at(-1).method, 'thread/unsubscribe'); await f.s.reopen();
  assert.deepEqual(await f.settle(), coverage);
});

test('inline-only Outlook content publishes an explicit original-content gap without an attachment-list completeness claim', { timeout: 90000 }, async t => {
  const f = await fixture(t, { message: { has_attachments: false, body: { contentType: 'html', content: '<p>Look here</p><img src="cid:original">' } } });
  const calls = f.n.state.dispatches.length, coverage = await f.settle();
  assert.equal(coverage.gap, 'media_inline_coverage_unavailable'); assert.deepEqual(coverage.attachments, []);
  assert.equal(f.n.state.dispatches.length, calls); assert.equal(f.gets, 0);
});

test('verified completed media archives release hot quota while replay keeps original bytes and performs no GET', { timeout: 90000 }, async t => {
  const f = await fixture(t), coverage = await f.settle();
  const published = await f.worker().publication.saveCoverage(coverage, f.plan); await f.worker().verifyCoverage(f.plan, published.value);
  f.s.spool.archivePublished(f.plan); assert.equal(f.s.spool.archiveStatus().records, 0);
  const gets = f.gets, calls = f.n.state.dispatches.length; await f.s.reopen();
  const retained = await f.worker().publication.coverage(f.plan.item); assert.deepEqual(retained, published);
  assert.deepEqual(await f.worker().publication.payload(retained.value.attachments[0].manifest, retained.value.admission), f.bytes);
  assert.equal(f.gets, gets); assert.equal(f.n.state.dispatches.length, calls);
});

test('native media journal binds account and exact attachment reads, publishes bytes, and retains explicit unsupported media', { timeout: 90000 }, async t => {
  const f = await fixture(t);
  f.n.state.media.metadata.attachments.push({ ...f.attachment, id: 'reference', attachment_type: 'referenceAttachment', odata_type: '#microsoft.graph.referenceAttachment', payload_fetch_supported: false });
  const coverage = await f.settle(); assert.equal(coverage.gap, null); assert.equal(coverage.attachments.length, 2);
  const complete = coverage.attachments[0]; assert.ok(complete.manifest); assert.equal(complete.errorCode, null);
  assert.equal(coverage.attachments[1].manifest, null); assert.equal(coverage.attachments[1].errorCode, 'media_attachment_unsupported');
  assert.equal(f.n.state.dispatches.at(-1).method, 'thread/unsubscribe'); assert.equal(f.gets, 1);
  assert.ok(!JSON.stringify(coverage).includes('protected-original'));
  const download = f.s.spool.get(deriveOperationId(f.s.spool.nativeCall(hashJson(f.plan), 4).call.operationId, 'download'));
  assert.deepEqual(await f.worker().publication.payload(complete.manifest, download.intent.admission), f.bytes);
  f.n.state.offline = true; await f.n.restart(); await f.s.reopen();
  assert.deepEqual(await f.worker().prepare(f.admission, f.plan.item, f.plan.captureOperationId), f.plan); assert.deepEqual(await f.settle(), coverage);
  assert.equal(f.gets, 1); assert.equal(f.n.state.dispatches.length, 15);
});

test('changed post-download account quarantines acquired bytes and publishes no attachment payload', { timeout: 90000 }, async t => {
  const f = await fixture(t);
  f.n.state.afterNative = operation => { if (operation.params.tool?.endsWith('fetch_attachment')) f.n.state.profile.email = 'other@example.test'; };
  const coverage = await f.settle(); assert.equal(coverage.gap, 'media_account_mismatch'); assert.equal(coverage.attachments[0].errorCode, 'media_account_mismatch');
  assert.equal(coverage.attachments[0].manifest, null); assert.equal((await f.n.objects('secretary/media-chunk')).length, 0); assert.equal(f.gets, 1);
  assert.equal(f.s.spool.get(deriveOperationId(f.s.spool.nativeCall(hashJson(f.plan), 4).call.operationId, 'download')).phase, 'acquired');
  assert.equal(f.n.state.dispatches.at(-1).method, 'thread/unsubscribe');
});

test('known pending native materialization survives reopen and refuses later absence before recovering its sole original GET', { timeout: 90000 }, async t => {
  const f = await fixture(t); f.n.state.pendingMethod = 'microsoft_outlook_email.fetch_attachment';
  for (let i = 0; i < 10 && !f.s.spool.nativeCall(hashJson(f.plan), 4)?.seen; i++) assert.equal((await f.worker().step(f.plan)).phase, 'pending');
  assert.ok(f.s.spool.nativeCall(hashJson(f.plan), 4)?.seen); assert.equal(f.gets, 0); const count = f.n.state.dispatches.length;
  const original = f.s.spool.nativeCall(hashJson(f.plan), 4), operation = f.n.state.operations.get(original.call.operationId);
  const accepted = structuredClone(operation);
  Object.assign(operation, { phase: 'outcome_unknown', epoch: f.n.state.epoch, requestId: 70, code: 'outcome_unknown' });
  assert.equal((await f.worker().step(f.plan)).phase, 'pending');
  assert.equal(f.s.spool.nativeCall(hashJson(f.plan), 4).seen.phase, 'outcome_unknown');
  await f.s.reopen();
  f.n.state.operations.set(operation.operationId, accepted);
  await assert.rejects(f.worker().step(f.plan), { code: 'native_evidence_regressed' });
  f.n.state.operations.set(operation.operationId, operation);
  await f.n.restart(); await f.s.reopen(); f.n.state.absent = true;
  await assert.rejects(f.worker().step(f.plan), { code: 'media_native_absence_conflict' }); assert.equal(f.n.state.dispatches.length, count);
  f.n.state.absent = false; Object.assign(operation, { phase: 'succeeded', code: null, reply: { result: f.n.state.results.get(operation.operationId) } }); f.n.state.pendingMethod = null;
  const coverage = await f.settle(); assert.equal(coverage.gap, null); assert.ok(coverage.attachments[0].manifest); assert.equal(f.gets, 1);
  assert.equal(f.n.state.dispatches.filter(value => value.params.tool?.endsWith('fetch_attachment')).length, 1);
});
