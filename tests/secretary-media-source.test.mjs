import test from 'node:test';
import assert from 'node:assert/strict';
import { IvyError } from '../dist/packages/sdk/src/node.js';
import { OutlookMediaSource } from '../dist/services/secretary/src/outlook-media-source.js';
import { mediaNativeFixture } from './secretary-media-native-fixture.mjs';
import { validateMedia } from '../dist/services/secretary/src/media-schema.js';

test('media source follows saved captures, retains coverage, and resumes the same collection after service recreation', { timeout: 90000 }, async t => {
  const f = await mediaNativeFixture(t); let gets = 0;
  const request = async () => { gets++; return new Response(f.bytes); };
  let source = new OutlookMediaSource(f.n.source, f.s.spool, request);
  assert.equal(await source.context(f.plan.item), null);
  assert.equal(await source.step(), 'pending'); const count = f.n.state.dispatches.length;
  await f.n.restart(); await f.s.reopen(); source = new OutlookMediaSource(f.n.source, f.s.spool, request);
  for (let i = 0; i < 40; i++) { const result = await source.step(); if (result === 'complete') break; assert.equal(result, 'pending'); }
  const coverage = await source.native.publication.coverage(f.plan.item); assert.ok(coverage); assert.equal(coverage.value.gap, null);
  assert.throws(() => validateMedia('MediaCoverage', { ...coverage.value, attachments: [coverage.value.attachments[0], coverage.value.attachments[0]] }));
  assert.throws(() => validateMedia('MediaCoverage', { ...coverage.value, gap: 'account_changed' }));
  assert.ok(coverage.value.attachments[0].manifest); assert.equal(gets, 1); assert.equal(source.issue, null); assert.ok(f.n.state.dispatches.length > count);
  f.n.state.offline = true; await f.n.restart(); await f.s.reopen(); source = new OutlookMediaSource(f.n.source, f.s.spool, async () => { throw Error('No repeated GET'); });
  assert.equal(await source.step(), 'idle'); assert.equal(gets, 1); assert.equal(f.n.state.dispatches.length, 15);
  assert.equal((await f.n.objects('secretary/media-coverage')).length, 1);
  const context = await source.context(f.plan.item); assert.deepEqual(context.coverage, coverage);
  const attachment = await context.readAttachment(0); assert.deepEqual(attachment.bytes, f.bytes);
  assert.deepEqual(attachment.pin, coverage.value.attachments[0].manifest);
  const prepared = await context.input(0, 1024 * 1024);
  assert.equal(prepared.kind, 'text');
  const projection = JSON.parse(prepared.input.text); assert.equal(projection.representation, 'document_text');
  assert.equal(projection.sections[0].text, 'inert'); assert.ok(projection.limitations.includes('embedded_media_not_interpreted'));
  assert.equal(prepared.source.contentHash, attachment.value.evidence.contentHash);
  assert.equal(prepared.source.byteLength, f.bytes.length);
  assert.equal(f.n.state.dispatches.length, 15); assert.equal(gets, 1);
  for (const index of [-1, 0.5, 1]) await assert.rejects(context.readAttachment(index), { code: 'media_context_mismatch' });
  context.coverage.value.attachments[0].manifest.objectId = 'caller-mutated-copy';
  assert.deepEqual((await context.readAttachment(0)).bytes, f.bytes);
});

test('lost media coverage response recovers one original record and the body checkpoint remains independent', { timeout: 90000 }, async t => {
  const f = await mediaNativeFixture(t), checkpoint = await f.n.f.engine.source('mail'); let gets = 0, hit = false;
  let source = new OutlookMediaSource(f.n.source, f.s.spool, async () => { gets++; return new Response(f.bytes); });
  f.n.state.after = async (method, _args, result) => {
    if (!hit && method === 'objects.write' && result.object.contractKey === 'secretary/media-coverage') { hit = true; f.n.state.cut = true; throw new IvyError('outcome_unknown', 'Lost saved media coverage.', 'unknown'); }
  };
  await assert.rejects((async () => { for (let i = 0; i < 40; i++) await source.step(); })()); assert.ok(hit);
  f.n.state.after = null; await f.n.restart(); await f.s.reopen(); f.n.state.offline = true;
  source = new OutlookMediaSource(f.n.source, f.s.spool, async () => { throw Error('No repeated GET'); });
  assert.equal(await source.step(), 'idle'); assert.equal(gets, 1); assert.equal((await f.n.objects('secretary/media-coverage')).length, 1);
  assert.deepEqual((await f.n.f.engine.source('mail')).pin, checkpoint.pin); assert.equal(f.n.state.dispatches.length, 15);
});

test("failed media coverage restores its diagnostic after recreation and does not repeat its GET", { timeout: 90000 }, async t => {
  const f = await mediaNativeFixture(t); let gets = 0;
  let source = new OutlookMediaSource(f.n.source, f.s.spool, async () => { gets++; return new Response("expired", { status: 403 }); });
  for (let i = 0; i < 40; i++) { if (await source.step() === "complete") break; }
  assert.equal(source.issue, "media_materialization_expired_or_refused");
  const saved = await source.native.publication.coverage(f.plan.item); assert.ok(saved); assert.equal(saved.value.attachments[0].manifest, null);
  f.n.state.offline = true; await f.n.restart(); await f.s.reopen();
  source = new OutlookMediaSource(f.n.source, f.s.spool, async () => { throw Error("No retry"); });
  assert.equal(await source.step(), "idle"); assert.equal(source.issue, "media_materialization_expired_or_refused"); assert.equal(gets, 1);
  const context = await source.context(f.plan.item); assert.equal(await context.readAttachment(0), null);
  assert.equal(context.coverage.value.attachments[0].errorCode, 'media_materialization_expired_or_refused');
});

test('final coverage cannot skip an unfinished native plan but remains readable domain context', { timeout: 45000 }, async t => {
  const f = await mediaNativeFixture(t), source = new OutlookMediaSource(f.n.source, f.s.spool, async () => { throw Error('No fabricated GET'); });
  await source.native.publication.saveCoverage({ schemaVersion: 1, admission: f.admission, observedAt: f.plan.createdAt, gap: null, attachments: [] }, f.plan);
  await assert.rejects(source.step(), { code: 'media_coverage_invalid' });
  const context = await source.context(f.plan.item); assert.deepEqual(context.coverage.value.attachments, []);
  assert.equal(f.n.state.dispatches.length, 7);
});
