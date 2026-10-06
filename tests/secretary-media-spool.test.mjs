import test from 'node:test';
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { DatabaseSync } from 'node:sqlite';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { digest } from '../dist/packages/sdk/src/node.js';
import { MediaSpool } from '../dist/services/secretary/src/media-spool.js';
import { materialization, intent, owner, spoolFixture } from './secretary-media-fixture.mjs';

test('media spool admits one concurrent GET and recovers identical committed bytes without any network', async t => {
  const f = await spoolFixture(t), bytes = Buffer.from('<b>original</b>');
  assert.equal(f.spool.prepare(intent, materialization).phase, 'prepared');
  await assert.rejects(MediaSpool.open(f.root, owner), { code: 'media_spool_owned' });
  let calls = 0, release, entered;
  const start = new Promise(resolve => { entered = resolve; }), wait = new Promise(resolve => { release = resolve; });
  const first = f.spool.acquire(intent.operationId, new AbortController().signal, async () => {}, async () => {
    calls++; entered(); await wait; return new Response(bytes, { headers: { 'Content-Length': String(bytes.length) } });
  });
  await start; assert.throws(() => f.spool.close(), { code: 'media_spool_busy' });
  assert.equal((await f.spool.acquire(intent.operationId, new AbortController().signal, async () => { throw Error('No second guard'); }, async () => { throw Error('No duplicate GET'); })).phase, 'downloading');
  release(); const acquired = await first; assert.equal(acquired.phase, 'acquired'); assert.equal(calls, 1); assert.equal(acquired.evidence.contentHash, digest(bytes));
  assert.ok(!JSON.stringify(acquired).includes('private-capability')); assert.ok(!JSON.stringify(acquired).includes('https://'));
  await f.reopen(); assert.deepEqual(f.spool.get(intent.operationId), acquired); assert.deepEqual(f.spool.payload(intent.operationId).bytes, bytes);
  assert.deepEqual(await f.spool.acquire(intent.operationId, new AbortController().signal, async () => { throw Error('Offline guard'); }, async () => { throw Error('Offline GET'); }), acquired);
  assert.deepEqual(f.spool.prepare(intent, materialization), acquired);
  assert.throws(() => f.spool.prepare({ ...intent, admission: { ...intent.admission, accountId: 'other' } }, materialization), { code: 'media_spool_intent_mismatch' });
  f.close(); await assert.rejects(MediaSpool.open(f.root, { ...owner, principalId: 'other' }), { code: 'media_spool_owner_changed' }); await f.reopen();
});

test('real child termination leaves an explicit interrupted attempt and cannot turn partial acquisition into a retry', { timeout: 15000 }, async t => {
  const f = await spoolFixture(t); f.spool.prepare(intent, materialization); f.close();
  const module = pathToFileURL(resolve('dist/services/secretary/src/media-spool.js')).href;
  const script = 'import {MediaSpool} from ' + JSON.stringify(module) + '; const spool=await MediaSpool.open(' + JSON.stringify(f.root) + ',' + JSON.stringify(owner) +
    '); await spool.acquire(' + JSON.stringify(intent.operationId) + ', new AbortController().signal, async()=>{}, async()=>{process.exit(42)});';
  const child = spawnSync(process.execPath, ['--input-type=module', '-e', script], { encoding: 'utf8', timeout: 10000, windowsHide: true });
  assert.equal(child.status, 42, child.stderr); await f.reopen();
  const failed = f.spool.get(intent.operationId); assert.equal(failed.phase, 'failed'); assert.equal(failed.errorCode, 'media_download_interrupted');
  assert.throws(() => f.spool.payload(intent.operationId), { code: 'media_spool_payload_unavailable' });
  assert.deepEqual(await f.spool.acquire(intent.operationId, new AbortController().signal, async () => { throw Error('No retry'); }), failed);
  await f.reopen(); assert.deepEqual(f.spool.get(intent.operationId), failed);
});

test('media quota is reserved before contact and a failed original attempt stays failed when a new explicit attempt is admitted', async t => {
  const f = await spoolFixture(t);
  // Metadata also counts against the quota, so sixteen full 32MiB reservations cannot fit.
  for (let i = 0; i < 15; i++) f.spool.prepare({ ...intent, operationId: 'reserved-' + i }, materialization);
  assert.throws(() => f.spool.prepare(intent, materialization), { code: 'media_spool_full' });
  let calls = 0;
  const failed = await f.spool.acquire('reserved-0', new AbortController().signal, async () => {}, async () => { calls++; return new Response('expired', { status: 403 }); });
  assert.equal(failed.phase, 'failed'); assert.equal(failed.errorCode, 'media_materialization_expired_or_refused');
  assert.equal(f.spool.prepare(intent, materialization).phase, 'prepared');
  assert.deepEqual(await f.spool.acquire('reserved-0', new AbortController().signal, async () => { throw Error('No retry'); }), failed); assert.equal(calls, 1);
});

test('media admission is fenced before download and unsupported storage formats are rejected', async t => {
  const f = await spoolFixture(t); f.spool.prepare(intent, materialization); let calls = 0;
  await assert.rejects(f.spool.acquire(intent.operationId, new AbortController().signal, async () => { throw Error('Owner lost'); }, async () => { calls++; return new Response('x'); }), /Owner lost/);
  assert.equal(calls, 0); assert.equal(f.spool.get(intent.operationId).phase, 'prepared');
  await f.spool.acquire(intent.operationId, new AbortController().signal, async () => {}, async () => new Response('original'));
  f.close(); const db = new DatabaseSync(join(f.root, 'media.sqlite'));
  db.exec('PRAGMA user_version=0'); db.close();
  await assert.rejects(f.reopen(), { code: 'media_spool_format_unsupported' });
});
