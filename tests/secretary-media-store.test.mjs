import test from 'node:test';
import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { deriveOperationId, hashJson, IvyError } from '../dist/packages/sdk/src/node.js';
import { mediaRegistry, MediaStore, verifyOutlookMediaAdmission } from '../dist/services/secretary/src/media-store.js';
import { outlookFixture, mail, page, sourceRegistry } from './secretary-outlook-fixture.mjs';
import { spoolFixture, materialization } from './secretary-media-fixture.mjs';

async function fixture(t, bytes = Buffer.alloc(1048576 + 23, 65)) {
  const n = await outlookFixture(t, () => { const source = sourceRegistry(), media = mediaRegistry();
    return { ...source, contracts: [...source.contracts, ...media.contracts], requiredContracts: [...source.requiredContracts, ...media.requiredContracts] }; });
  n.state.pages.set(0, page([mail('message', { has_attachments: true })]));
  assert.equal(await n.settle(), 'complete');
  const head = await n.source.head(), proof = await n.source.evidence(head.value.lastPage);
  const publication = await n.source.store.find('secretary/outlook-publication', 'outlook-publication-' + hashJson(proof.pin).slice(7));
  const capture = await n.f.engine.store.read('secretary/operation', publication.value.captured[0]), item = await n.f.engine.item(capture.value.effect);
  const admission = { sourceId: 'mail', producerPrincipalId: n.f.settings.identity.principalId, accountId: n.configuration.profile.id, providerMessageId: 'message',
    itemObjectId: item.pin.objectId, identityHash: item.value.identityHash, messageHash: item.value.messageHash };
  const intent = { operationId: deriveOperationId(capture.value.operationId, 'media'), captureOperationId: capture.value.operationId, item: item.pin, admission, materializationHash: hashJson(materialization) };
  const s = await spoolFixture(t); s.spool.prepare(intent, materialization);
  const store = () => new MediaStore(n.f.engine, (value, captureOperationId) => verifyOutlookMediaAdmission(n.source, value, captureOperationId));
  await s.spool.acquire(intent.operationId, new AbortController().signal, () => verifyOutlookMediaAdmission(n.source, admission, intent.captureOperationId),
    async () => new Response(bytes, { headers: { 'Content-Length': String(bytes.length) } }));
  assert.equal(s.spool.get(intent.operationId).phase, 'acquired');
  return { n, s, intent, admission, store, bytes };
}

test('media publication binds a real saved source capture to immutable bounded chunks and recovers all bytes offline', { timeout: 45000 }, async t => {
  const f = await fixture(t), saved = await f.store().publish(f.s.spool, f.intent.operationId);
  assert.equal(saved.value.chunks.length, 2); assert.equal(saved.pin.revision, 1);
  assert.deepEqual(await f.store().payload(saved.pin, f.admission), f.bytes);
  assert.ok(!JSON.stringify(saved.value).includes('private-capability'));
  f.n.state.offline = true; await f.n.restart(); await f.s.reopen();
  assert.deepEqual(await f.store().publish(f.s.spool, f.intent.operationId), saved);
  assert.deepEqual(await f.store().payload(saved.pin, f.admission), f.bytes);
  assert.equal((await f.n.objects('secretary/media-chunk')).length, 2); assert.equal((await f.n.objects('secretary/media-manifest')).length, 1);
  assert.equal(f.n.state.dispatches.length, 7);
});

test('lost chunk and manifest replies recover their original mutations with no additional native calls or GET', { timeout: 90000 }, async t => {
  for (const key of ['secretary/media-chunk', 'secretary/media-manifest']) {
    const f = await fixture(t); let hit = false;
    f.n.state.after = async (method, _args, value) => {
      if (!hit && method === 'objects.write' && value.object.contractKey === key) { hit = true; f.n.state.cut = true; throw new IvyError('outcome_unknown', 'Lost committed media response.', 'unknown'); }
    };
    await assert.rejects(f.store().publish(f.s.spool, f.intent.operationId)); assert.ok(hit); f.n.state.after = null;
    f.n.state.offline = true; await f.n.restart(); await f.s.reopen();
    const saved = await f.store().publish(f.s.spool, f.intent.operationId);
    assert.deepEqual(await f.store().payload(saved.pin, f.admission), f.bytes);
    assert.equal((await f.n.objects('secretary/media-chunk')).length, 2); assert.equal((await f.n.objects('secretary/media-manifest')).length, 1);
    assert.equal(f.n.state.dispatches.length, 7); assert.equal(f.s.spool.get(f.intent.operationId).phase, 'acquired');
  }
});

test('media refuses foreign source, capture or item identities before publication and rejects later rewritten chunks', { timeout: 45000 }, async t => {
  const f = await fixture(t);
  for (const changed of [{ accountId: 'other' }, { producerPrincipalId: 'other' }, { providerMessageId: 'other' }, { identityHash: hashJson('other') }, { itemObjectId: randomUUID() }]) {
    await assert.rejects(verifyOutlookMediaAdmission(f.n.source, { ...f.admission, ...changed }, f.intent.captureOperationId));
  }
  await assert.rejects(verifyOutlookMediaAdmission(f.n.source, f.admission, deriveOperationId(f.intent.captureOperationId, 'foreign')));
  assert.equal((await f.n.objects('secretary/media-chunk')).length, 0);
  const saved = await f.store().publish(f.s.spool, f.intent.operationId), chunk = saved.value.chunks[0];
  await f.n.f.connection.request('objects.write', { mutationId: f.n.f.operationId(), references: {}, objectId: chunk.objectId, expectedRevision: 1, contractVersion: '1.0.0',
    content: { encoding: 'base64', value: Buffer.from('changed').toString('base64') } });
  await assert.rejects(f.store().payload(saved.pin, f.admission), { code: 'media_publication_invalid' });
  await assert.rejects(f.store().publish(f.s.spool, f.intent.operationId), { code: 'media_publication_invalid' });
});

test('a valid empty attachment publishes an exact zero-byte manifest without inventing a chunk', { timeout: 45000 }, async t => {
  const f = await fixture(t, Buffer.alloc(0)), saved = await f.store().publish(f.s.spool, f.intent.operationId);
  assert.deepEqual(saved.value.chunks, []); assert.equal(saved.value.evidence.byteLength, 0);
  assert.deepEqual(await f.store().payload(saved.pin, f.admission), Buffer.alloc(0));
});
