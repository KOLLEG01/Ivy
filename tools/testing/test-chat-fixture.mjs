// Native Chat/Agent processes on the caller's isolated Hive. Only the external
// native protocol is synthetic; notices and acknowledgments use public RPCs.
import assert from 'node:assert/strict';
import { after } from 'node:test';
import { randomUUID } from 'node:crypto';
import { copyFile, mkdir, mkdtemp, readFile, rm, symlink } from 'node:fs/promises';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
import { setTimeout as delay } from 'node:timers/promises';
import { nativeAgentFixture } from '../../tests/fixtures/native-agent.mjs';
import { artifactRoot, startNativeService, writeJson } from './native-service.mjs';
import { NativeContract } from '../../dist/packages/contracts/src/native-contract.js';
import { saveNativePlan } from '../../dist/packages/sdk/src/native-plan.js';
import { HiveClient, discover, callBound, newOperationId } from '../../dist/packages/sdk/src/client.js';

const roots = [];
after(async () => { for (const root of roots) await rm(root, { recursive: true, force: true, maxRetries: 3 }); });

export async function testChatFixture(t) {
  const base = process.env.IVY_TEST_HIVE_URL, credential = process.env.IVY_TEST_HIVE_CREDENTIAL;
  assert.ok(base && credential, 'Run with the isolated native Hive harness.');
  assert.equal(new URL(base).hostname, '127.0.0.1');
  const root = await mkdtemp(join(tmpdir(), 'ivy-native-chat-fixture-'));
  roots.push(root);
  const client = new HiveClient(base, { credential }), principal = 'isolated-consumer-test';
  let agent, chat;
  let closing;
  const close = () => closing ??= (async () => {
    try {
      if (chat) {
        assert.deepEqual(await chat.close(), { code: 0, signal: null });
        assert.equal(JSON.parse(await readFile(join(chat.config.dataRoot, 'health.json'), 'utf8')).ready, false);
      }
    } finally {
      await agent?.close();
      if (agent) assert.equal(JSON.parse(await readFile(join(agent.config.dataRoot, 'health.json'), 'utf8')).ready, false);
    }
  })();
  t.after(close);
  agent = await nativeAgentFixture(null, { root, base, credential, client }, { completeCatalog: true, serviceTasks: true });
  const artifact = join(root, 'chat-artifact'), selected = artifactRoot('chat-bridge');
  await mkdir(join(artifact, 'dist/specs/native/codex-' + agent.catalog.version), { recursive: true });
  await copyFile(join(selected, 'dist/build-info.json'), join(artifact, 'dist/build-info.json'));
  for (const name of ['native', 'instructions'])
    await symlink(join(selected, 'dist', name), join(artifact, 'dist', name), process.platform === 'win32' ? 'junction' : 'dir');
  // The fixture executable has a different identity. Both native consumers load
  // the same complete catalog, including unchanged method schemas.
  await writeJson(join(artifact, 'dist/specs/native/codex-' + agent.catalog.version + '/catalog.json'), agent.catalog);
  const contract = new NativeContract(agent.catalog), methods = { threadStart: 'thread/start', threadResume: 'thread/resume', turnStart: 'turn/start', turnInterrupt: 'turn/interrupt' };
  const draft = { nativeVersion: agent.catalog.version, catalogSourceHash: agent.catalog.sourceHash,
    definitions: Object.fromEntries(Object.entries(methods).map(([key, method]) => [key, contract.definitionHash(method)])),
    threadStart: { cwd: agent.nativeProjects[0].path }, threadResume: {}, turnStart: {} };
  const nativePlan = await saveNativePlan(client, contract, { draft, kind: 'chat', parentId: null, mutationId: await newOperationId(client) });
  const channel = { adapter: 'whatsapp', accountId: 'fixture-account', channelId: randomUUID() };
  const definition = { workspaceId: randomUUID(), principalId: principal, rootObjectId: null,
    project: { serviceNodeId: agent.config.serviceNodeId, namespace: 'codex', kind: 'project', nativeId: agent.nativeProjects[0].id }, nativePlan,
    channels: [{ channel, displayName: 'Isolated native Main' }] };
  const config = { schemaVersion: 1, componentId: 'chat-bridge', instanceId: 'chat', serviceNodeId: 'native-chat-' + randomUUID(), hostId: 'fixture',
    publicBaseUrl: base, dataRoot: join(root, 'chat-data'), credential, settings: { definition, pollMs: 1000 } };
  const call = async (name, args) => callBound(client, await discover(client, 'chat.' + name, { serviceNodeId: config.serviceNodeId }), args, args.operationId);
  const until = async (observe, label) => {
    const end = Date.now() + 25000;
    while (Date.now() < end) { const value = await observe(); if (value) return value; await delay(50); }
    throw Error('Native Chat did not reach ' + label + ': ' + chat?.logs);
  };
  const start = async () => {
    chat = await startNativeService(config, { artifact });
    await until(async () => { const node = await client.request('serviceNodes.get', { serviceNodeId: config.serviceNodeId }); return node.connected && node.synced && node.ready; }, 'public readiness');
  };
  await start();
  const workspace = await call('workspace', {}), expectedBridge = workspace.expectedBridge;
  const created = await call('createMain', { action: 'createMain', operationId: await newOperationId(client), expectedBridge,
    expectedMainRevision: null, reason: 'Create the isolated integration Main.' });
  assert.equal(created.phase, 'succeeded');
  const receive = async () => call('receive', { action: 'receive', operationId: await newOperationId(client), expectedBridge, channel, afterSequence: 0, limit: 8 });
  return { root, base, credential, principal, client, agent, channel, expectedBridge, call, receive, until, close,
    config, catalog: agent.catalog, get owner() { return chat; },
    restart: async () => { await chat.close(); await start(); },
    acknowledge: async offer => call('acknowledge', { action: 'acknowledge', operationId: await newOperationId(client), expectedBridge,
      offerId: offer.outcome.offer.object.objectId, replyIds: offer.outcome.replies.map(reply => reply.object.objectId) }) };
}
