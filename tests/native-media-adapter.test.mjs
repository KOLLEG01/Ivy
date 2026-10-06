import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, readFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { nativeMediaAdapter } from './acceptance/support/native-media-adapter.mjs';
const body = { sdp: 'v=0\r\n', session: { model: 'native-default', instructions: 'Preserve this text.', voice: 'cove', delegation: { type: 'client' } } };
const request = (adapter, value = body, headers = {}) => fetch(adapter.baseUrl + '/realtime/calls?intent=quicksilver&architecture=avas', { method: 'POST', headers: { authorization: 'Bearer isolated-canary-credential', 'content-type': 'application/json', 'openai-alpha': 'quicksilver=v2', ...headers }, body: JSON.stringify(value) });

test('media adapter removes only native model and replays one atomic response across concurrent retry and restart', async t => {
  const directory = await mkdtemp(join(tmpdir(), 'ivy-media-')); let calls = 0;
  const send = async (url, init) => {
    calls++; assert.equal(url, 'https://chatgpt.com/backend-api/codex/realtime/calls?intent=quicksilver&architecture=avas'); assert.equal(init.redirect, 'error');
    assert.equal(init.headers.authorization, 'Bearer isolated-canary-credential'); assert.equal(init.headers['openai-alpha'], 'quicksilver=v2');
    assert.deepEqual(JSON.parse(init.body), { sdp: body.sdp, session: { instructions: body.session.instructions, voice: 'cove', delegation: { type: 'client' } } });
    return new Response('v=answer\r\n', { status: 201, headers: { 'content-type': 'application/sdp', location: '/v1/live/rtc_test' } });
  };
  let adapter = await nativeMediaAdapter(directory, send); t.after(() => adapter.close());
  const replies = await Promise.all([request(adapter), request(adapter)]); for (const reply of replies) { assert.equal(reply.status, 201); assert.equal(await reply.text(), 'v=answer\r\n'); assert.equal(reply.headers.get('location'), '/v1/live/rtc_test'); }
  assert.equal(calls, 1); assert.equal(adapter.summary().response.status, 201);
  const saved = await readFile(join(directory, 'media-receipt.json'), 'utf8'); assert.equal(saved.includes('isolated-canary-credential'), false); assert.equal(saved.includes('Bearer '), false);
  await adapter.close(); adapter = await nativeMediaAdapter(directory, () => { throw Error('Replay cannot send.'); }); assert.equal((await request(adapter)).status, 201);
  assert.equal((await request(adapter, { ...body, sdp: 'different' })).status, 409); assert.equal((await request(adapter, body, { authorization: 'Bearer another-account' })).status, 409); assert.equal(calls, 1);
});

test('media adapter retains an uncertain dispatch and does not repeat it after native retry or restart', async t => {
  const directory = await mkdtemp(join(tmpdir(), 'ivy-media-unknown-')); let calls = 0;
  let adapter = await nativeMediaAdapter(directory, async () => { calls++; throw Error('Lost provider response.'); }); t.after(() => adapter.close());
  assert.equal((await request(adapter)).status, 409); assert.equal((await request(adapter)).status, 409); assert.equal(calls, 1); assert.equal(adapter.summary().state, 'outcome_unknown');
  await adapter.close(); adapter = await nativeMediaAdapter(directory, async () => { throw Error('Must not repeat.'); }); assert.equal((await request(adapter)).status, 409);
});

test('media adapter rejects browser origins missing native auth malformed requests and foreign paths before forwarding', async t => {
  const directory = await mkdtemp(join(tmpdir(), 'ivy-media-boundary-')); let calls = 0;
  const adapter = await nativeMediaAdapter(directory, async () => { calls++; throw Error('No admission expected.'); }); t.after(() => adapter.close());
  assert.equal((await request(adapter, body, { origin: 'https://example.test' })).status, 403);
  assert.equal((await request(adapter, body, { authorization: '' })).status, 403);
  assert.equal((await request(adapter, body, { cookie: 'session=foreign' })).status, 403);
  assert.equal((await request(adapter, { ...body, session: {} })).status, 400);
  assert.equal((await request(adapter, { ...body, extra: 'not-native' })).status, 400);
  assert.equal((await fetch(adapter.baseUrl + '/other')).status, 404); assert.equal(calls, 0); assert.equal(adapter.summary(), null);
});
