import { unpersistedNativePlan } from './fixtures/native-plan.js';
import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { randomUUID } from 'node:crypto';
import { digest } from '../packages/contracts/src/canonical.js';
import { validateChat } from '../packages/contracts/src/validation.js';
import type { Agent, Chat, OperationName, Params, Result, Wire } from '../packages/contracts/src/generated.js';
import { ChatAdmission } from '../services/chat-bridge/src/admission.js';
import { admittedNativeInput } from '../services/chat-bridge/src/input-media.js';
import { chatContracts } from '../services/chat-bridge/src/schema.js';
import { conversationName, inputName, mainName } from '../services/chat-bridge/src/queue.js';
import { HiveKernel } from '../services/hive/src/kernel.js';
import type { ConnectionContext } from '../services/hive/src/kernel.js';
import type { RpcClient } from '../packages/sdk/src/client.js';
import { newOperationId } from '../packages/sdk/src/client.js';
import catalog from '../specs/schemas/chat.operations.json' with { type: 'json' };

// Actual one-pixel PNG/JPEG/WebP files encoded once with Pillow; no decoder is mocked as native.
const png = Buffer.from('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAIAAACQd1PeAAAADElEQVR4nGMQ0bABAADMAHm9FcOvAAAAAElFTkSuQmCC', 'base64');
const jpeg = Buffer.from('/9j/4AAQSkZJRgABAQAAAQABAAD/2wBDAAgGBgcGBQgHBwcJCQgKDBQNDAsLDBkSEw8UHRofHh0aHBwgJC4nICIsIxwcKDcpLDAxNDQ0Hyc5PTgyPC4zNDL/2wBDAQkJCQwLDBgNDRgyIRwhMjIyMjIyMjIyMjIyMjIyMjIyMjIyMjIyMjIyMjIyMjIyMjIyMjIyMjIyMjIyMjIyMjL/wAARCAABAAEDASIAAhEBAxEB/8QAHwAAAQUBAQEBAQEAAAAAAAAAAAECAwQFBgcICQoL/8QAtRAAAgEDAwIEAwUFBAQAAAF9AQIDAAQRBRIhMUEGE1FhByJxFDKBkaEII0KxwRVS0fAkM2JyggkKFhcYGRolJicoKSo0NTY3ODk6Q0RFRkdISUpTVFVWV1hZWmNkZWZnaGlqc3R1dnd4eXqDhIWGh4iJipKTlJWWl5iZmqKjpKWmp6ipqrKztLW2t7i5usLDxMXGx8jJytLT1NXW19jZ2uHi4+Tl5ufo6erx8vP09fb3+Pn6/8QAHwEAAwEBAQEBAQEBAQAAAAAAAAECAwQFBgcICQoL/8QAtREAAgECBAQDBAcFBAQAAQJ3AAECAxEEBSExBhJBUQdhcRMiMoEIFEKRobHBCSMzUvAVYnLRChYkNOEl8RcYGRomJygpKjU2Nzg5OkNERUZHSElKU1RVVldYWVpjZGVmZ2hpanN0dXZ3eHl6goOEhYaHiImKkpOUlZaXmJmaoqOkpaanqKmqsrO0tba3uLm6wsPExcbHyMnK0tPU1dbX2Nna4uPk5ebn6Onq8vP09fb3+Pn6/9oADAMBAAIRAxEAPwDyOiiiuw5D/9k=', 'base64');
const webp = Buffer.from('UklGRjAAAABXRUJQVlA4ICQAAABQAQCdASoBAAEAAUAmJQBOgCgAAP76id+R2EN2HLri5shvAAA=', 'base64');
const channel: Chat.WhatsAppChannelConfiguration['channel'] = { adapter: 'whatsapp', accountId: 'account', channelId: 'channel' };
function definition(version: '0.154.0' = '0.154.0'): Chat.Definition {
  const native = JSON.parse(readFileSync('specs/native/codex-' + version + '/catalog.json', 'utf8')) as Agent.Catalog;
  return { workspaceId: 'chat', principalId: 'chat-service', rootObjectId: null,
    project: { serviceNodeId: 'owner', namespace: 'codex', kind: 'project', nativeId: '/synthetic/project' },
    nativePlan: unpersistedNativePlan(version),
    channels: [{ channel: structuredClone(channel), displayName: 'Original channel' }] };
}
const send = (admission: ChatAdmission, images: Chat.Image[] = []): Chat.SendRequest => ({ action: 'send', operationId: 'operation-one',
  expectedBridge: admission.expected('alice'), channel: structuredClone(channel), messageId: 'message-one',
  expectedBinding: { objectId: 'binding-one', revision: 1 }, payload: { text: 'Synthetic image input', images } });
const code = (expected: string) => (error: unknown) => (error as { code: string }).code === expected;

test('exact ChatBridge sender, account, channel and configuration admission precedes every storage access', async () => {
  const admission = new ChatAdmission(definition()); let accesses = 0;
  const client: RpcClient = { async request() { accesses++; throw new Error('No storage operation may occur.'); } };
  const request = send(admission);
  for (const [caller, changed] of [
    ['Alice', request], ['bob', request],
    ['alice', { ...request, channel: { ...channel, accountId: 'Account' } }],
    ['alice', { ...request, expectedBridge: { ...request.expectedBridge, callerPrincipalId: 'bob' } }],
  ] as const) await assert.rejects(admittedNativeInput(admission, client, caller, changed), error => ['chat_sender_refused', 'chat_definition_mismatch'].includes((error as { code: string }).code));
  await assert.rejects(admittedNativeInput(admission, client, 'alice', { ...request, channel: { ...channel, adapter: 'unconfigured-output' } } as unknown as Chat.SendRequest));
  assert.equal(accesses, 0);
  assert.deepEqual(admission.channelsFor('alice').map(value => value.channel.channelId), ['channel']);
  const copied = admission.channelsFor('alice'); copied[0]!.displayName = 'Changed copy';
  assert.equal(admission.channelsFor('mallory')[0]!.displayName, 'Original channel');
  assert.equal(admission.expected('mallory').callerPrincipalId, 'mallory');
});

test('ChatBridge definition and message identities stay exact across transport retries and cannot be edited after admission', () => {
  const source = definition(), admission = new ChatAdmission(source), request = send(admission), original = admission.send('alice', request);
  source.channels[0]!.displayName = 'Changed source'; assert.equal(admission.channelsFor('mallory')[0]!.displayName, 'Original channel');
  assert.throws(() => { admission.definition.workspaceId = 'changed'; });
  assert.equal(admission.send('alice', { ...request, operationId: 'a-new-transport-operation' }).requestHash, original.requestHash);
  for (const changed of [{ ...request, payload: { text: 'different', images: [] } }, { ...request, expectedBinding: { ...request.expectedBinding!, revision: 2 } }]) {
    assert.deepEqual(admission.send('alice', changed).identity, original.identity);
    assert.notEqual(admission.send('alice', changed).requestHash, original.requestHash);
  }
  assert.throws(() => new ChatAdmission({ ...source, channels: [source.channels[0]!, source.channels[0]!] }), code('invalid_arguments'));
  assert.throws(() => new ChatAdmission({ ...definition(), project: { ...definition().project, kind: 'thread' } }), code('chat_definition_mismatch'));
  assert.throws(() => admission.send('alice', { ...request, payload: { text: '', images: [] } }), code('invalid_arguments'));
  assert.doesNotThrow(() => admission.send('alice', { ...request, payload: { text: '🧪'.repeat(65536), images: [] } }));
  assert.throws(() => admission.send('alice', { ...request, payload: { text: '🧪'.repeat(65537), images: [] } }));
});

test('each immutable ChatBridge definition receives separate aggregate identities', () => {
  const first = definition(), second = structuredClone(first), identity: Chat.InputIdentity = { channel, senderPrincipalId: 'alice', messageId: 'message-one' };
  second.channels[0]!.displayName = 'Renamed channel';
  assert.notEqual(mainName(first), mainName(second));
  assert.notEqual(conversationName(first), conversationName(second));
  assert.notEqual(inputName(first, identity), inputName(second, identity));
  assert.equal(mainName(first), mainName(structuredClone(first)));
});

async function fixture(t: { after(callback: () => void): void }) {
  const credentialDigest = digest('synthetic-chat-media');
  const kernel = new HiveKernel({ filename: ':memory:', publicBaseUrl: 'https://chat-fixture.test/ivy', version: 'test', buildId: digest('chat-fixture'), credentials: [{ principalId: 'chat-service', digest: credentialDigest }] });
  t.after(() => kernel.close()); let context: ConnectionContext = { credentialDigest, principalId: 'chat-service', transport: 'ws' }, reads = 0;
  const execute = <M extends OperationName>(method: M, params: Params<M>): Result<M> => {
    const result = kernel.execute(context, { jsonrpc: '2.0', id: randomUUID(), method, params });
    assert.equal(result.kind, 'result'); if (result.kind !== 'result') throw Error('No native dispatch in media admission tests.'); return result.value as Result<M>;
  };
  const connected = execute('service.connect', { serviceNodeId: 'chat-node', serviceName: 'chat-bridge', hostId: 'fixture', version: 'test', buildId: digest('chat-node'), hiveProtocol: 1 });
  context = { ...context, serviceNodeId: 'chat-node', generation: connected.generation };
  execute('registry.sync', { namespaces: [], contracts: [...chatContracts().filter(value => value.mediaType.startsWith('image/')), {
    key: 'fixture/chat-owner', version: '1.0.0', owner: { kind: 'service', serviceName: 'chat-bridge' }, mediaType: 'application/json',
    retention: { objects: { mode: 'retain' }, revisions: { mode: 'current' } }, jsonSchema: { type: 'object', additionalProperties: false },
    specMarkdown: 'Synthetic retained Chat conversation owner.' }], requiredContracts: [] });
  execute('service.heartbeat', { ready: true, diagnostics: [] });
  const client: RpcClient = { async request<M extends OperationName>(method: M, params: Params<M>) { if (method === 'objects.read') reads++; return execute(method, params); } };
  const owner = await client.request('objects.write', { mutationId: await newOperationId(client), contractVersion: '1.0.0', references: {},
    create: { contractKey: 'fixture/chat-owner', parentId: null, ownerObjectId: null, name: 'Chat conversation' }, content: { encoding: 'json', value: {} } });
  const image = async (mediaType: Chat.Image['mediaType'], bytes: Buffer): Promise<Chat.Image> => {
    const saved = await client.request('objects.write', { mutationId: await newOperationId(client), contractVersion: '1.0.0',
      references: {}, create: { contractKey: catalog.mediaContracts.find(value => value.mediaType === mediaType)!.key, parentId: null, ownerObjectId: owner.object.id, name: randomUUID() }, content: { encoding: 'base64', value: bytes.toString('base64') } });
    return { object: { objectId: saved.object.id, revision: saved.revision.revision }, mediaType, contentHash: digest(bytes), byteLength: bytes.length, label: 'Synthetic pixel' };
  };
  return { client, image, reads: () => reads, collect: () => kernel.retention.collect() };
}

test('actual Hive image revisions produce exact native image arguments for the supported native schema', async t => {
  const f = await fixture(t);
  for (const version of ['0.154.0'] as const) {
    const admission = new ChatAdmission(definition(version));
    for (const [mediaType, bytes] of [['image/png', png], ['image/jpeg', jpeg], ['image/webp', webp]] as const) {
      const image = await f.image(mediaType, bytes), request = send(admission, [image]);
      const value = await admittedNativeInput(admission, f.client, 'alice', request);
      assert.deepEqual(value.input, [{ type: 'text', text: request.payload.text }, { type: 'image', url: 'data:' + mediaType + ';base64,' + bytes.toString('base64') }]);
      validateChat('NativeRequest', { nativeVersion: version, catalogSourceHash: admission.definition.nativePlan.catalogSourceHash, method: 'turn/start', params: { threadId: 'synthetic-main', input: value.input } });
    }
  }
  assert.equal(f.reads(), 3);
});

test('superseded image revisions are unavailable under current-only retention', async t => {
  const f = await fixture(t), admission = new ChatAdmission(definition()), image = await f.image('image/png', png), request = send(admission, [image]);
  await f.client.request('objects.write', { mutationId: await newOperationId(f.client), objectId: image.object.objectId, expectedRevision: 1, contractVersion: '1.0.0', references: {}, content: { encoding: 'base64', value: Buffer.from('new invalid bytes').toString('base64') } });
  f.collect(); f.collect();
  const client: RpcClient = { async request<M extends OperationName>(method: M, params: Params<M>) {
    request.payload.text = 'changed while awaiting storage'; request.payload.images = [];
    return f.client.request(method, params);
  } };
  await assert.rejects(admittedNativeInput(admission, client, 'alice', request), code('revision_pruned'));
});

test('media admission rejects wrong hashes/contracts and malformed exact container bytes from real Hive storage', async t => {
  const f = await fixture(t), admission = new ChatAdmission(definition()), image = await f.image('image/png', png);
  for (const changed of [{ ...image, contentHash: digest('wrong') }, { ...image, byteLength: image.byteLength + 1 }, { ...image, mediaType: 'image/jpeg' as const }])
    await assert.rejects(admittedNativeInput(admission, f.client, 'alice', send(admission, [changed])), code('chat_image_mismatch'));
  const highBit = Buffer.from(webp); highBit[0] = highBit[0]! | 128;
  const trailing = Buffer.concat([webp, Buffer.from([0, 0])]);
  const chunkLength = Buffer.from(webp); chunkLength.writeUInt32LE(0xffffffff, 16);
  for (const [mediaType, bytes] of [['image/png', Buffer.from('not PNG')], ['image/jpeg', Buffer.from('not JPEG')], ['image/webp', highBit], ['image/webp', trailing], ['image/webp', chunkLength]] as const) {
    const invalid = await f.image(mediaType, bytes);
    await assert.rejects(admittedNativeInput(admission, f.client, 'alice', send(admission, [invalid])), code('chat_image_invalid'));
  }
});
