import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { mkdir, readFile, writeFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { parseArgs } from 'node:util';
import { fileURLToPath } from 'node:url';
import { HiveClient, callBound, discover, hashJson, validateAgent } from '../../../dist/packages/sdk/src/node.js';
import { atomicJson } from '../../../dist/packages/sdk/src/host.js';
import { startSecretary } from '../../../dist/services/secretary/src/main.js';
import { SecretaryEngine } from '../../../dist/services/secretary/src/engine.js';
import { TeamsSource } from '../../../dist/services/secretary/src/teams-source.js';
import { microsoftProfile } from '../../../dist/services/secretary/src/outlook-projection.js';

// One explicitly bound unread Teams scope in a new acceptance root. This does not
// assess, mark read, send, run a model, or control the existing native host/browser.
const { values } = parseArgs({ options: { 'host-config': { type: 'string' }, 'state-dir': { type: 'string' }, 'expected-host': { type: 'string' },
  'agent-instance': { type: 'string' }, 'binding-file': { type: 'string' }, since: { type: 'string' }, 'recovery-only': { type: 'boolean' } } });
for (const name of ['host-config', 'state-dir', 'expected-host', 'agent-instance', 'binding-file', 'since']) assert.ok(values[name], 'Missing --' + name);
const directory = resolve(values['state-dir']), host = JSON.parse(await readFile(resolve(values['host-config']), 'utf8'));
assert.equal(host.hostId, values['expected-host']); assert.ok(host.hostId.endsWith('-acceptance')); await mkdir(directory, { recursive: true });
const binding = JSON.parse(await readFile(resolve(values['binding-file']), 'utf8')); assert.deepEqual(microsoftProfile(binding.profile), binding.profile);
assert.match(binding.expectedAccountHash, /^sha256:[0-9a-f]{64}$/);
const agent = host.instances.find(item => item.instanceId === values['agent-instance']); assert.equal(agent?.componentId, 'agent-manager'); assert.ok(agent.credential);
const client = new HiveClient(host.publicBaseUrl, { credential: agent.credential }), owner = await client.request('serviceNodes.get', { serviceNodeId: agent.serviceNodeId });
let seed;
try { seed = JSON.parse(await readFile(join(directory, 'identity.json'), 'utf8')); }
catch (error) {
  if (error.code !== 'ENOENT') throw error; assert.ok(!values['recovery-only']);
  const status = await callBound(client, await discover(client, 'agent.status', { serviceNodeId: agent.serviceNodeId }), {}); validateAgent('Status', status); assert.equal(status.state, 'ready');
  seed = { schemaVersion: 1, id: randomUUID(), base: host.publicBaseUrl, hostId: host.hostId, principalId: owner.principalId, bindingHash: hashJson(binding), since: values.since,
    target: Object.fromEntries(['serviceNodeId', 'hostId', 'nativeVersion', 'nativeExecutableHash', 'catalogHash'].map(key => [key, status[key]])) };
  await writeFile(join(directory, 'identity.json'), JSON.stringify(seed, null, 2) + '\n', { flag: 'wx', mode: 0o600 });
}
assert.equal(seed.base, host.publicBaseUrl); assert.equal(seed.hostId, host.hostId); assert.equal(seed.principalId, owner.principalId); assert.equal(seed.since, values.since);
assert.equal(seed.target.serviceNodeId, agent.serviceNodeId); assert.equal(seed.bindingHash, hashJson(binding));
const rootContract = { key: 'secretary-teams-acceptance/root', version: '1.0.0', owner: { kind: 'agent' }, mediaType: 'application/json', jsonSchema: { type: 'object', additionalProperties: false }, specMarkdown: 'Protected actual read-only Teams acceptance, without assessment, sending or changes to existing native hosts.' };
await client.request('contracts.register', { mutationId: 'teams-root-contract-' + hashJson(rootContract).slice(7), definition: rootContract });
const root = await client.request('objects.write', { mutationId: 'teams-root-' + seed.id, contractVersion: '1.0.0', create: { contractKey: rootContract.key, parentId: null, name: seed.id }, content: { encoding: 'json', value: {} } });
const settings = { identity: { scope: { secretaryId: seed.id, rootObjectId: root.object.id }, principalId: seed.principalId, serviceNodeId: host.hostId + '.teams-' + seed.id, hostId: host.hostId },
  sources: [{ sourceId: 'teams', accountId: binding.profile.id, kind: 'teams', producerPrincipalIds: [seed.principalId], ownSenderIds: ['user:' + binding.profile.id], allowedSenderIds: null }],
  policy: { silentTime: null, urgentBypass: false, whatsappQuestionsOnly: true },  pollMs: 500, recordsPerTick: 2 };
const collector = { sourceId: 'teams', target: seed.target, expectedAccountHash: binding.expectedAccountHash, profile: binding.profile, since: seed.since,
  maximumChats: 32, maximumMessages: 32, excludedChatIds: [], pollMs: 3600000, threadCwd: join(directory, 'project') };
await mkdir(collector.threadCwd, { recursive: true });
const build = JSON.parse(await readFile(new URL('../../../dist/build-info.json', import.meta.url), 'utf8')), dataRoot = join(directory, 'data'), path = join(directory, 'instance.json');
const config = { schemaVersion: 1, instanceId: seed.id, componentId: 'secretary', serviceNodeId: settings.identity.serviceNodeId, hostId: host.hostId, publicBaseUrl: host.publicBaseUrl,
  credential: agent.credential, dataRoot, artifactRoot: fileURLToPath(new URL('../../../', import.meta.url)), version: build.version, buildId: build.buildId, settings };
await atomicJson(path, config); await atomicJson(join(directory, 'config.json'), {schemaVersion:1,teams:{ schemaVersion: 1, collectors: values['recovery-only'] ? [] : [collector] }});
// Recovery mode disables automatic native work and validates the retained proof
// through the same running service's authenticated owner using an offline facade.
const report = { schemaVersion: 1, id: seed.id, root: root.object.id, build, target: seed.target, startedAt: new Date().toISOString(), recoveryOnly: Boolean(values['recovery-only']), phase: 'starting',
  nativeModel: false, externalWrites: false, readMarkersChanged: false, installedRuntimeOwner: false, browserChanges: false };
let service;
try {
  service = await startSecretary(path); const connection = await service.service.waitReady({ timeoutMs: 30000 });
  const offline = { request(method, args, options) { assert.ok(!['tools.call', 'tools.list'].includes(method), 'Recovery must not query or dispatch native tools.'); return connection.request(method, args, options); } };
  const engine = new SecretaryEngine(offline, settings, { serviceNodeId: settings.identity.serviceNodeId, generation: connection.generation }, connection.signal), source = new TeamsSource(engine, collector);
  const deadline = Date.now() + 600000; let head;
  do {
    head = await source.head(); if (!head.value.pending && head.value.lastPage && !head.value.continuation) break;
    assert.ok(!values['recovery-only'] && Date.now() < deadline, 'Original Teams collection is still pending; resume the same state directory.');
    await new Promise(resolve => setTimeout(resolve, 500));
  } while (true);
  const proof = await source.evidence(head.value.lastPage); assert.equal(proof.value.gap, null); assert.ok(proof.value.page); assert.ok(head.value.lastCompleteAt);
  const checkpoint = await engine.source('teams'); assert.match(checkpoint.value.cursor, /^teams-page:/);
  report.phase = 'teams_checkpoint_confirmed'; report.head = head.pin; report.evidence = proof.pin; report.checkpoint = checkpoint.pin;
  report.pages = []; report.calls = []; report.messageCount = 0; report.messagesWithAttachments = 0;
  const hashes = [], originalOperations = new Set();
  for (let revision = 3; revision <= head.pin.revision; revision += 2) {
    const saved = await source.store.read('secretary/teams-head', { objectId: head.pin.objectId, revision }); assert.equal(saved.value.pending, null);
    const evidence = await source.evidence(saved.value.lastPage); assert.equal(evidence.value.gap, null); assert.ok(evidence.value.page);
    const page = evidence.value.page, pageHash = hashJson(page.messages); hashes.push(pageHash); report.messageCount += page.messages.length; report.messagesWithAttachments += page.messagesWithAttachments;
    report.pages.push({ evidence: evidence.pin, kind: page.selection.chat ? 'messages' : 'chats', chatCount: page.chats.length, messageCount: page.messages.length, messagesHash: pageHash, observedAt: page.observedAt, excluded: page.excluded });
    assert.equal(evidence.value.calls.length, 7);
    for (const pin of evidence.value.calls) {
      const call = await source.store.read('secretary/teams-call', pin); assert.ok(!originalOperations.has(call.value.operationId)); originalOperations.add(call.value.operationId);
      report.calls.push({ method: call.value.method, operationId: call.value.operationId, observationHash: call.value.observation.contentHash });
    }
    assert.equal(report.calls.at(-1).method, 'thread/unsubscribe');
  }
  assert.equal(report.pages[0].kind, 'chats'); assert.equal(report.pages.length, report.pages[0].chatCount + 1);
  report.messagesHash = hashJson(hashes); report.observedAt = proof.value.observedAt;
  const original = { head: report.head, evidence: report.evidence, checkpoint: report.checkpoint, messagesHash: report.messagesHash, pages: report.pages, calls: report.calls };
  try { assert.deepEqual(JSON.parse(await readFile(join(directory, 'baseline.json'), 'utf8')), original); }
  catch (error) { if (error.code !== 'ENOENT') throw error; assert.ok(!values['recovery-only']); await atomicJson(join(directory, 'baseline.json'), original); }
} catch (error) { report.phase = 'failed'; report.failure = { code: error.code ?? error.name, message: error.message }; process.exitCode = 1; }
finally { if (service) await service.close(); report.finishedAt = new Date().toISOString(); await atomicJson(join(directory, values['recovery-only'] ? 'recovery-report.json' : 'report.json'), report);
  process.stdout.write(JSON.stringify({ phase: report.phase, directory, messageCount: report.messageCount, failure: report.failure ?? null }) + '\n'); }
