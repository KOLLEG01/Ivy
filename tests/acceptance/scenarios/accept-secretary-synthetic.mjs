import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { mkdir, readFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { parseArgs } from 'node:util';
import { setTimeout as delay } from 'node:timers/promises';
import { HiveClient, discover, callBound, hashJson } from '../../../dist/packages/sdk/src/node.js';
import { atomicJson } from '../../../dist/packages/sdk/src/host.js';
import { startSecretary } from '../../../dist/services/secretary/src/main.js';

// Synthetic inputs are supplied from private evidence, never from real accounts. The caller owns
// the already authenticated isolated AgentManager. This runner owns only its Secretary instance.
const { values } = parseArgs({ options: Object.fromEntries(['host-config', 'owner', 'project-path', 'cases', 'evidence'].map(key => [key, { type: 'string' }])) });
for (const key of ['host-config', 'owner', 'project-path', 'cases', 'evidence']) assert.ok(values[key], 'Missing --' + key);
const host = JSON.parse(await readFile(values['host-config'], 'utf8'));
assert.ok(host.hostId.endsWith('-acceptance')); assert.equal(new URL(host.publicBaseUrl).hostname, '127.0.0.1');
const token = host.instances.find(item => item.componentId === 'hive')?.settings.credentials[0]?.token; assert.ok(token);
const cases = JSON.parse(await readFile(values.cases, 'utf8'));
assert.ok(Array.isArray(cases) && cases.length > 0 && cases.length <= 6);
assert.equal(new Set(cases.map(item => item.id)).size, cases.length);
for (const item of cases) {
  assert.ok(typeof item.id === 'string' && item.id.length < 100);
  assert.ok(['email', 'teams', 'whatsapp'].includes(item.kind)); assert.ok(typeof item.text === 'string');
  assert.ok(item.expected && Array.isArray(item.expected.dispositions) && item.expected.dispositions.length);
  if (item.expected.noticeState !== undefined) {
    assert.ok(['confirmed', 'suppressed'].includes(item.expected.noticeState));
    assert.ok(!item.expected.mediaPending, 'Untriaged media cannot have a notice expectation');
  }
}
const directory = resolve(values.evidence); await mkdir(directory);
const client = new HiveClient(host.publicBaseUrl, { credential: token });
const status = await client.request('system.status', {}), principalId = status.callerPrincipalId;
const call = async (node, name, args, operationId = randomUUID()) => callBound(client, await discover(client, name, { serviceNodeId: node }), args, operationId, { timeoutMs: 45000 });
const native = await call(values.owner, 'agent.status', {}); assert.equal(native.state, 'ready');
const models = await call(values.owner, 'codex.model/list', { limit: 100 });
assert.ok(models.data.some(model => model.model === 'gpt-5.6-luna' && !model.hidden && model.supportedReasoningEfforts.some(option => option.reasoningEffort === 'high')));
const id = randomUUID(), rootContract = { key: 'secretary-synthetic-acceptance/root', version: '1.0.0', owner: { kind: 'agent' }, mediaType: 'application/json', jsonSchema: { type: 'object', additionalProperties: false }, specMarkdown: 'Isolated synthetic acceptance root.' };
await client.request('contracts.register', { mutationId: id + '-contract', definition: rootContract });
const created = await client.request('objects.write', { mutationId: id + '-root', contractVersion: '1.0.0', create: { contractKey: rootContract.key, parentId: null, name: id }, content: { encoding: 'json', value: {} } });
const scope = { secretaryId: id, rootObjectId: created.object.id }, serviceNodeId = 'synthetic-secretary-' + id;
const settings = { identity: { scope, principalId, serviceNodeId, hostId: host.hostId },
  sources: ['email', 'teams', 'whatsapp'].map(kind => ({ sourceId: kind, accountId: 'synthetic-' + kind, kind, producerPrincipalIds: [principalId], allowedSenderIds: null, ownSenderIds: ['self@example.test'] })),
  policy: { silentTime: null, urgentBypass: false, whatsappQuestionsOnly: true }, pollMs: 500, recordsPerTick: 2 };
const target = Object.fromEntries(['serviceNodeId', 'hostId', 'nativeVersion', 'nativeExecutableHash', 'catalogHash'].map(key => [key, native[key]]));
const triage = { schemaVersion: 1, target, model: 'gpt-5.6-luna', effort: 'high', maximumConcurrent: 1, threadCwd: resolve(values['project-path']),
  instructions: 'Bewerte nur die vorgelegte Nachricht. Melde konkrete Fragen, Fristen und wichtige Informationen knapp auf Deutsch. Werbung und belanglosen Smalltalk nicht melden. Keine Inhalte erfinden, keine Nachrichten senden und keine Werkzeuge verwenden. Fehlende Anhänge ausdrücklich als fehlend behandeln.' };
const artifactRoot = fileURLToPath(new URL('../../../', import.meta.url)), build = JSON.parse(await readFile(join(artifactRoot, 'dist/build-info.json'), 'utf8'));
const dataRoot = join(directory, 'data'); await mkdir(dataRoot);
await atomicJson(join(directory, 'config.json'), {schemaVersion:1,triage});
const configPath = join(directory, 'instance.json');
await atomicJson(configPath, { schemaVersion: 1, componentId: 'secretary', instanceId: id, serviceNodeId, hostId: host.hostId, publicBaseUrl: host.publicBaseUrl, credential: token, artifactRoot, dataRoot, version: build.version, buildId: build.buildId, settings });
const report = { schemaVersion: 1, phase: 'starting', build, target, scope, casesHash: hashJson(cases), startedAt: new Date().toISOString(),
  budget: { model: triage.model, effort: triage.effort, maximumModelTurns: cases.filter(item => !item.expected.mediaPending).length, concurrency: 1, perCaseTimeoutMs: 180000 }, items: [], externalEffects: false };
const save = () => atomicJson(join(directory, 'report.json'), report); await save();
let secretary;
try {
  secretary = await startSecretary(configPath); await secretary.service.waitReady({ timeoutMs: 30000 });
  for (const item of cases) {
    const at = new Date().toISOString(), operationId = id + '-' + item.id;
    const message = { accountId: 'synthetic-' + item.kind, conversationId: item.id, messageId: item.id, nativeRevision: 'synthetic-1', senderId: 'sender@example.test', outgoing: false,
      attachments: item.attachments ?? 'none', occurredAt: item.occurredAt ?? at, observedAt: at, title: item.title ?? item.id, text: item.text, url: 'https://example.test/synthetic/' + encodeURIComponent(item.id) };
    const request = { action: 'capture', operationId, expectedScope: scope, sourceId: item.kind, message };
    const capture = await call(serviceNodeId, 'secretary.capture', request, operationId); assert.equal(capture.phase, 'succeeded');
    assert.deepEqual(await call(serviceNodeId, 'secretary.capture', request, operationId), capture, 'Duplicate original capture must return the original receipt');
    const deadline = Date.now() + report.budget.perCaseTimeoutMs; let assessed;
    if (item.expected.mediaPending) {
      assert.equal(message.attachments, 'expected');
      const pendingDeadline = Date.now() + 15000;
      const issueKey = 'triage:' + capture.effect.objectId;
      while (!secretary.engine?.recoveryIssues.has(issueKey) && Date.now() < pendingDeadline) await delay(250);
      assert.equal(secretary.engine?.recoveryIssues.get(issueKey), 'secretary_triage_media_pending');
      const original = await client.request('objects.read', { objectId: capture.effect.objectId });
      assert.equal(original.content.value.decision, null);
      const work = await client.request('objects.query', { contractKey: 'secretary/triage-work', limit: 10, where: { op: 'and', args: [{ op: 'eq', field: 'object.parentId', value: scope.rootObjectId }, { op: 'eq', field: 'data:/item/objectId', value: capture.effect.objectId }] } });
      assert.equal(work.items.length, 0); assert.equal(work.nextCursor, null);
      report.items.push({ id: item.id, capture: capture.effect, state: 'media_pending', modelTurnStarted: false }); await save();
      continue;
    }
    while (Date.now() < deadline) {
      assessed = await client.request('objects.read', { objectId: capture.effect.objectId });
      if (assessed.content.value.decision) break;
      const expectedPending = new Set(report.items.filter(value => value.state === 'media_pending').map(value => 'triage:' + value.capture.objectId));
      const unexpectedIssues = [...(secretary.engine?.recoveryIssues ?? [])].filter(([key, code]) => code !== 'secretary_triage_media_pending' || !expectedPending.has(key));
      assert.equal(unexpectedIssues.length, 0, JSON.stringify(unexpectedIssues));
      await delay(1000);
    }
    const decision = assessed.content.value.decision; assert.ok(decision, 'Triage deadline: ' + item.id);
    const works = await client.request('objects.query', { contractKey: 'secretary/triage-work', limit: 10, where: { op: 'and', args: [{ op: 'eq', field: 'object.parentId', value: scope.rootObjectId }, { op: 'eq', field: 'data:/item/objectId', value: capture.effect.objectId }] } });
    assert.equal(works.items.length, 1); assert.equal(works.nextCursor, null);
    const work = (await client.request('objects.read', { objectId: works.items[0].objectId })).content.value;
    // The assessment write precedes the final work receipt by one reconciliation step.
    let finished = work;
    while (finished.phase !== 'assessed' && Date.now() < deadline) { await delay(500); finished = (await client.request('objects.read', { objectId: works.items[0].objectId })).content.value; }
    assert.equal(finished.phase, 'assessed');
    const completion = (await client.request('objects.read', finished.completion)).content.value;
    assert.deepEqual(completion.result, decision.assessment);
    const thread = await call(values.owner, 'codex.thread/read', { threadId: completion.threadId, includeTurns: true });
    const turn = thread.thread.turns.find(turn => turn.id === completion.turnId); assert.equal(turn.status, 'completed');
    const finals = turn.items.filter(part => part.type === 'agentMessage' && part.phase === 'final_answer'); assert.equal(finals.length, 1);
    assert.deepEqual(JSON.parse(finals[0].text), decision.assessment);
    const entry = { id: item.id, capture: capture.effect, completion: finished.completion, threadId: completion.threadId, turnId: completion.turnId, assessment: decision.assessment };
    report.items.push(entry); await save();
    assert.ok(item.expected.dispositions.includes(decision.assessment.disposition), 'Unexpected disposition: ' + item.id);
    if (item.expected.question !== undefined) assert.equal(decision.assessment.question, item.expected.question);
    for (const pattern of item.expected.summaryPatterns ?? []) assert.match(decision.assessment.summary, new RegExp(pattern, 'iu'));
    if (item.expected.noticeState !== undefined) {
      let observed;
      while (Date.now() < deadline) {
        observed = await client.request('objects.read', { objectId: capture.effect.objectId });
        assert.deepEqual(observed.content.value.decision, decision, 'Notice delivery must preserve the original triage decision');
        if (observed.content.value.notice?.state === item.expected.noticeState) break;
        await delay(500);
      }
      const notice = observed?.content.value.notice;
      entry.notice = notice; await save(); assert.equal(notice?.state, item.expected.noticeState);
      if (item.expected.noticeState === 'confirmed') {
        assert.ok(notice.target); assert.ok(notice.evidence && notice.request);
        entry.noticeEvidence = await client.request('objects.read', notice.evidence);
        assert.equal(entry.noticeEvidence.object.contractKey, 'chat-bridge/reply');
        assert.equal(entry.noticeEvidence.content.encoding, 'json');
        const reply = entry.noticeEvidence.content.value;
        assert.equal(reply.state, 'confirmed'); assert.equal(reply.origin.kind, 'native');
        const handoff = await client.request('objects.read', { objectId: reply.origin.inputId });
        assert.equal(handoff.content.value.identity.senderPrincipalId, principalId);
        assert.equal(handoff.content.value.operationId, notice.request.operationId);
        assert.deepEqual(handoff.content.value.result, reply.origin.result);
        assert.deepEqual(handoff.content.value.binding, reply.origin.binding);
        assert.deepEqual(reply.channel, notice.target.channel);
        assert.equal(entry.noticeEvidence.revision.contentHash, hashJson(reply));
        await save();
      }
    }
  }
  const expectedTurns = cases.filter(item => !item.expected.mediaPending).length;
  assert.equal(new Set(report.items.filter(item => item.turnId).map(item => item.turnId)).size, expectedTurns);
  report.modelTurns = expectedTurns;
  report.phase = 'passed';
} catch (error) { report.phase = 'failed'; report.failure = { code: error.code ?? error.name, message: error.message }; process.exitCode = 1; }
finally { try { await secretary?.close(); } finally { report.finishedAt = new Date().toISOString(); await save(); } }
console.log(JSON.stringify({ phase: report.phase, report: join(directory, 'report.json'), failure: report.failure }));
