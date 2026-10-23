import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, rmSync } from 'node:fs';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
import type { WASocket } from 'baileys';
import { ChatFiles, outgoingFileBytes } from '../services/chat-bridge/src/files.js';
import { ChatBridge } from '../services/chat-bridge/src/bridge.js';
import { WhatsAppJournal } from '../services/chat-bridge/src/whatsapp/journal.js';
import { WhatsAppAuthStore } from '../services/chat-bridge/src/whatsapp/auth.js';
import { WhatsAppSocket } from '../services/chat-bridge/src/whatsapp/socket.js';
import { chatNativeFixture } from './fixtures/chat-native.js';
import { digest } from '../packages/contracts/src/canonical.js';
import { validateChat } from '../packages/contracts/src/chat-validation.js';
import type { Chat, Wire } from '../packages/contracts/src/generated.js';

const png = Buffer.from('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+jN1kAAAAASUVORK5CYII=', 'base64');
const file = (bytes = Buffer.from('Harmless file\n'), mediaType = 'text/plain', filename = 'harmless.txt') =>
  ({ filename, mediaType, bytesBase64: bytes.toString('base64'), byteLength: bytes.length, contentHash: digest(bytes) });

async function fixture(t: test.TestContext) {
  const f = await chatNativeFixture(t), bridge = new ChatBridge(f.first);
  const created = await f.first.create('alice', f.request('file-main'));
  assert.equal(created.phase, 'succeeded');
  const root = mkdtempSync(join(tmpdir(), 'ivy-chat-files-')), path = join(root, 'journal.sqlite');
  let journal = new WhatsAppJournal(path);
  const auth = new WhatsAppAuthStore(join(root, 'auth.sqlite'));
  const config = { accountId: f.channel.accountId, botPhoneNumber: '+15555550100', since: '2026-09-10T00:00:00.000Z',
    senders: [{ phoneNumber: '+15555550200', principalId: 'alice', channelId: f.channel.channelId }] };
  let files = new ChatFiles(journal, config, () => bridge); bridge.files = files;
  t.after(async () => { await journal.close(); auth.close(); rmSync(root, { recursive: true, force: true }); });
  const target = await files.target('alice');
  return { f, bridge, root, config, auth, target,
    get journal() { return journal; }, get files() { return files; },
    request: (id: string, selected = file()): Chat.SendFileRequest => ({ action: 'sendFile', operationId: f.operation(id),
      expectedBridge: target.expectedBridge, expectedBinding: target.expectedBinding, file: selected, caption: null }),
    async reopen() { await journal.close(); journal = new WhatsAppJournal(path); files = new ChatFiles(journal, config, () => bridge); bridge.files = files; },
    socket() { const socket = new WhatsAppSocket(config, journal, auth, root, () => {}); socket.files = files; socket.connected = true; return socket; },
  };
}

test('outgoing content checks exact bytes and MIME; unsupported image formats remain original documents', async () => {
  assert.equal((await outgoingFileBytes(file(png, 'image/png', 'picture.png'))).kind, 'image');
  const gif = Buffer.from('R0lGODlhAQABAIAAAAAAAP///ywAAAAAAQABAAACAUwAOw==', 'base64');
  assert.equal((await outgoingFileBytes(file(gif, 'image/gif', 'picture.gif'))).kind, 'document');
  assert.equal((await outgoingFileBytes(file(Buffer.from('<svg xmlns="http://www.w3.org/2000/svg"/>'), 'image/svg+xml', 'picture.svg'))).kind, 'document');
  assert.deepEqual((await outgoingFileBytes(file())).bytes, Buffer.from('Harmless file\n'));
  for (const selected of [
    { ...file(), bytesBase64: file().bytesBase64 + '\n' }, { ...file(), byteLength: 1 }, { ...file(), contentHash: digest('other') },
    file(png, 'text/plain'), file(Buffer.from('plain text'), 'image/png'), file(Buffer.from([0, 255, 1]), 'text/plain'),
    file(Buffer.from('{broken'), 'application/json'), file(Buffer.alloc(8 * 1024 * 1024 + 1)),
  ]) await assert.rejects(outgoingFileBytes(selected));
  const largeImage = Buffer.concat([png, Buffer.alloc(4 * 1024 * 1024)]);
  await assert.rejects(outgoingFileBytes(file(largeImage, 'image/png')), { code: 'limit_exceeded' });
  const { bytesBase64: _bytes, ...metadata } = file();
  for (const filename of ['../other', 'folder\\other', 'line\nfeed']) assert.throws(() => validateChat('OutgoingFile', { ...metadata, filename }));
});

test('the explicit API fences generation, caller and Main, deduplicates concurrent requests and hides bytes from receipts', async t => {
  const x = await fixture(t), request = x.request('api'), context = { callerPrincipalId: 'alice', generation: x.f.first.native.owner.generation,
    signal: new AbortController().signal, operationId: request.operationId };
  const target = await x.bridge.invoke('file', { fileTarget: true }, context);
  assert.deepEqual(target, x.target);
  await assert.rejects(x.bridge.invoke('sendFile', request as unknown as Record<string, Wire.Json>, { ...context, generation: context.generation + 1 }), { code: 'stale_generation' });
  for (const changed of [
    { ...request, expectedBridge: { ...request.expectedBridge, callerPrincipalId: 'bob' } },
    { ...request, expectedBridge: { ...request.expectedBridge, workspaceId: 'elsewhere' } },
    { ...request, expectedBridge: { ...request.expectedBridge, definitionHash: digest('wrong') } },
    { ...request, expectedBinding: { ...request.expectedBinding, objectId: 'another-main' } },
  ]) await assert.rejects(x.bridge.invoke('sendFile', changed as unknown as Record<string, Wire.Json>, context));
  assert.equal(x.journal.outgoing().length, 0);
  const results = await Promise.all([x.bridge.invoke('sendFile', request as unknown as Record<string, Wire.Json>, context),
    x.bridge.invoke('sendFile', request as unknown as Record<string, Wire.Json>, context)]);
  assert.deepEqual(results[0], results[1]); assert.equal(x.journal.outgoing().length, 1);
  assert.equal((results[0] as unknown as Chat.FileDelivery).state, 'queued');
  assert.ok(!JSON.stringify(results).includes('bytesBase64'));
  await assert.rejects(x.files.send('alice', { ...request, caption: 'changed' }), { code: 'chat_identity_conflict' });
  await assert.rejects(x.files.read('bob', { fileOperationId: request.operationId }), { code: 'not_found' });
  await assert.rejects(x.bridge.invoke('sendFile', { ...request, operationId: x.f.request('file-main').operationId } as unknown as Record<string, Wire.Json>,
    { ...context, operationId: x.f.request('file-main').operationId }), { code: 'chat_identity_conflict' });
});

test('file bytes survive restart, share text order and require an actual transport acknowledgement', async t => {
  const x = await fixture(t), image = x.request('image', file(png, 'image/png', 'tiny.png'));
  const queued = await x.files.send('alice', image), doc = x.request('document', file(Buffer.from('ä\r\nExact original bytes\n'), 'text/plain', 'Grüße.txt'));
  const document = await x.files.send('alice', doc);
  x.journal.enqueue('following-text', '15555550200@s.whatsapp.net', 'Following answer');
  await x.reopen();
  const socket = x.socket(), calls: Array<{ id: string; content: Record<string, unknown> }> = [];
  socket.socket = { async sendMessage(jid: string, content: Record<string, unknown>, options: { messageId: string }) {
    calls.push({ id: options.messageId, content });
    return { key: { id: options.messageId, remoteJid: jid, fromMe: true }, message: { conversation: 'test envelope' }, status: 1 };
  } } as unknown as WASocket;
  await socket.flush();
  assert.deepEqual(calls.map(c => c.id), [queued.messageId]);
  assert.deepEqual(calls[0]!.content['image'], png);
  assert.equal((await x.files.read('alice', { fileOperationId: image.operationId }) as Chat.FileDelivery).state, 'sending');
  await socket.confirmDelivery({ id: queued.messageId, remoteJid: '15555550300@s.whatsapp.net', fromMe: true }, 2, socket.socket!);
  assert.equal((await x.files.read('alice', { fileOperationId: image.operationId }) as Chat.FileDelivery).state, 'sending');
  await socket.confirmDelivery({ id: queued.messageId, remoteJid: '15555550200@s.whatsapp.net', fromMe: true }, 2, socket.socket!);
  await socket.flush();
  assert.equal(calls[1]!.content['fileName'], 'Grüße.txt');
  assert.deepEqual(calls[1]!.content['document'], Buffer.from(doc.file.bytesBase64, 'base64'));
  assert.equal(calls.length, 2);
  (socket.socket as unknown as Record<string, unknown>)['signalRepository'] = { lidMapping: { getPNForLID: async () => '15555550200@s.whatsapp.net' } };
  await socket.confirmDelivery({ id: document.messageId, remoteJid: '555000@lid', fromMe: true }, 3, socket.socket!); await socket.flush();
  assert.equal(calls[2]!.content['text'], 'Following answer');
  await x.reopen();
  assert.equal((await x.files.send('alice', doc)).state, 'sent');
  assert.equal(x.journal.db.prepare("SELECT COUNT(*) AS n FROM kv WHERE key LIKE 'file:%' AND json_extract(value,'$.bytesBase64') IS NOT NULL").get()!['n'], 0);
});

test('lost confirmation and interrupted media dispatch never resend, while late acknowledgement reconciles the same ID', async t => {
  const x = await fixture(t), request = x.request('lost'), queued = await x.files.send('alice', request);
  let sends = 0;
  const socket = x.socket(); socket.socket = { async sendMessage() { sends++; throw Error('Lost response'); } } as unknown as WASocket;
  await socket.flush(); await socket.flush();
  assert.equal(sends, 1); assert.equal((await x.files.send('alice', request)).state, 'outcome_unknown');
  await x.reopen();
  const restarted = x.socket(); restarted.socket = socket.socket;
  await restarted.flush(); assert.equal(sends, 1);
  x.files.mark(queued.messageId, 'sent');
  assert.equal((await x.files.send('alice', request)).state, 'sent');
  const interrupted = x.request('interrupted'), second = await x.files.send('alice', interrupted);
  x.files.mark(second.messageId, 'sending'); await x.reopen();
  const recovered = x.socket(); recovered.socket = socket.socket;
  await recovered.flush(); assert.equal(sends, 1);
  assert.equal((await x.files.send('alice', interrupted)).state, 'outcome_unknown');
});

test('queued attachments refuse changed Main/recipient and storage corruption before any socket send, and expiry retains replay fences', async t => {
  const x = await fixture(t), changed = x.request('changed'), queued = await x.files.send('alice', changed);
  x.config.senders[0]!.phoneNumber = '+15555550300';
  const socket = x.socket(); let sends = 0;
  socket.socket = { async sendMessage() { sends++; } } as unknown as WASocket;
  await socket.flush(); assert.equal(sends, 0);
  assert.equal((await x.files.read('alice', { fileOperationId: changed.operationId }) as Chat.FileDelivery).state, 'failed');
  x.config.senders[0]!.phoneNumber = '+15555550200';
  const corrupt = x.request('corrupt'), next = await x.files.send('alice', corrupt);
  const key = x.journal.get<string>('out-file:' + next.messageId)!;
  const saved = x.journal.get<Record<string, unknown>>(key)!; saved['bytesBase64'] = Buffer.from('Changed bytes').toString('base64'); x.journal.set(key, saved);
  await socket.flush(); assert.equal(sends, 0);
  assert.equal((await x.files.read('alice', { fileOperationId: corrupt.operationId }) as Chat.FileDelivery).state, 'failed');
  const mainChanged = x.request('main-changed'); await x.files.send('alice', mainChanged);
  const workspace = await x.bridge.workspace('alice');
  x.bridge.workspace = async () => ({ ...workspace, binding: { ...workspace.binding!, object: { objectId: 'replacement', revision: 1 } } });
  await socket.flush(); assert.equal(sends, 0);
  assert.equal((await x.files.read('alice', { fileOperationId: mainChanged.operationId }) as Chat.FileDelivery).state, 'failed');
  x.files.prune(Date.now() + 8 * 86400_000); await x.reopen();
  await assert.rejects(x.files.send('alice', changed), { code: 'operation_expired' });
  assert.equal(x.journal.get('out-file:' + queued.messageId), null);
});
