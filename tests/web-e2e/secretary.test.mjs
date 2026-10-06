import { assignmentFor } from '../secretary-fixture.mjs';
import test from 'node:test';
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { resolve } from 'node:path';
import { randomUUID } from 'node:crypto';
import { expect } from '@playwright/test';
import { ServiceClient, digest, IvyError, newOperationId } from '../../dist/packages/sdk/src/node.js';
import { contractVersion, secretaryRegistry } from '../../dist/services/secretary/src/schema.js';
import { defaultConfiguration } from '../../dist/services/secretary/src/assignment-schema.js';
import { publishUi } from '../../dist/packages/cli/src/publish-ui.js';
import { fixture, fixtureToken } from './fixtures/fixture.mjs';

for (const viewport of [{ width: 1440, height: 900 }, { width: 390, height: 844 }]) {
  test(`Secretary loads complete document pages without individual reads at ${viewport.width}px`, { timeout: 90000 }, async t => {
    const f = await fixture(t, false, viewport);
    const agentStates = [];
    const agent = new ServiceClient({ publicBaseUrl: f.base, credential: () => fixtureToken,
      identity: { serviceNodeId: 'browser-agent', serviceName: 'agent-manager', hostId: 'fixture', version: 'test', buildId: digest('browser-agent'), hiveProtocol: 1 },
      registry: () => ({ namespaces: [{ namespace: 'agent', description: 'Execution provider fixture', guideMarkdown: '', topics: [], inventoryKinds: [],
        tools: [{ namespace: 'agent', name: 'status', interfaceVersion: '1.0.0', description: 'Read execution provider readiness.',
          inputSchema: { type: 'object', additionalProperties: false }, outputSchema: { type: 'object' }, annotations: { readOnlyHint: true } }] }], contracts: [], requiredContracts: [] }),
      handlers: { 'agent.status': () => ({}) }, reconcile: async () => {}, onState: state => agentStates.push(state) });
    t.after(() => agent.stop()); agent.start();
    await agent.waitReady({ timeoutMs: 10000 }).catch(cause => { throw new Error(JSON.stringify(agentStates.slice(-6)), { cause }); });
    const rootContract = { key: 'fixture/secretary-root', version: '1.0.0', owner: { kind: 'agent' }, mediaType: 'application/json',
      retention: { objects: { mode: 'retain' }, revisions: { mode: 'current' } }, jsonSchema: { type: 'object' }, specMarkdown: 'Isolated Secretary browser fixture.' };
    await f.client.request('contracts.register', { definition: rootContract, mutationId: await newOperationId(f.client) });
    const root = (await f.client.request('objects.write', { mutationId: await newOperationId(f.client), contractVersion: '1.0.0', references: {},
      create: { contractKey: rootContract.key, parentId: null, ownerObjectId: null, name: 'secretary-root' }, content: { encoding: 'json', value: {} } })).object.id;
    const scope = { secretaryId: 'browser-secretary', rootObjectId: root }, registry = secretaryRegistry(), at = '2026-09-22T08:00:00.000Z';
    registry.namespaces[0].topics = [{ topic: 'secretary.example', version: '1.0.0', title: 'Example event', description: 'An event from a registered provider.',
      payloadSchema: { type: 'object', properties: { recordId: { type: 'string' } }, required: ['recordId'], additionalProperties: false },
      eventKinds: [{ kind: 'record.changed', title: 'Record changed', description: 'A record needs review.' }] }];
    const handlers = Object.fromEntries(registry.namespaces.flatMap(namespace => namespace.tools.map(tool => [namespace.namespace + '.' + tool.name, () => { throw Error('This fixture only supports reads.'); }])));
    const saves = [];
    handlers['secretary.saveAssignment'] = args => { saves.push(structuredClone(args)); throw new IvyError('fixture_capture_only', 'Save captured without changing the fixture.'); };
    handlers['secretary.binding'] = (_args, context) => ({ serviceNodeId: 'browser-secretary', hostId: 'fixture', available: true, expectedScope: scope,
      generation: context.generation, bindingHash: digest('browser-secretary-binding'), observedAt: at });
    const secretaryStates = [];
    const secretary = new ServiceClient({ publicBaseUrl: f.base, credential: () => fixtureToken,
      identity: { serviceNodeId: 'browser-secretary', serviceName: 'secretary', hostId: 'fixture', version: 'test', buildId: digest('browser-secretary'), hiveProtocol: 1 },
      registry: () => registry, handlers, reconcile: async () => {}, onState: state => secretaryStates.push(state) });
    t.after(() => secretary.stop()); secretary.start();
    const connection = await secretary.waitReady({ timeoutMs: 10000 }).catch(cause => { throw new Error(JSON.stringify(secretaryStates.slice(-6)), { cause }); });
    const savedIds = new Set();
    const save = async (contractKey, contractVersion, name, value) => {
      const saved = await connection.request('objects.write', { mutationId: await newOperationId(connection), contractVersion, references: {},
        create: { contractKey, parentId: root, ownerObjectId: null, name }, content: { encoding: 'json', value } });
      savedIds.add(saved.object.id); return { objectId: saved.object.id, revision: saved.revision.revision };
    };
    const configuration = defaultConfiguration({ policy: { minimumMainUrgency: 'normal', silentTime: null, researchMaxMinutes: 5 } }, at);
    await save('secretary/configuration', '1.0.0', 'configuration', configuration);
    await publishUi(f.client, { directory: resolve('dist/apps/secretary-ui'), definition: JSON.parse(await readFile('ui/secretary-ui/ui.json', 'utf8')),
      expectedReleaseId: null, mutationId: await newOperationId(f.client) });
    await f.login();
    const fits = async () => assert.ok(await f.page.evaluate(() => document.documentElement.scrollWidth <= innerWidth + 2), 'The Secretary page must fit its viewport.');
    await f.page.goto(f.base + '/ui/secretary-ui/#/journal');
    await f.page.getByRole('link', { name: /Explore what Secretary can do|Möglichkeiten ansehen/ }).waitFor({ timeout: 5000 })
      .catch(async cause => { throw new Error(await f.page.locator('body').innerText(), { cause }); });
    await f.page.getByRole('link', { name: /Explore what Secretary can do|Möglichkeiten ansehen/ }).click();
    await f.page.getByRole('button', { name: /^(Draft a schedule|Zeitplan entwerfen)$/ }).click();
    await expect(f.page.locator('#assignment-name')).toHaveValue(/^(Regular overview|Regelmäßiger Überblick)$/);
    await expect(f.page.locator('#notification-age')).toHaveValue('0');
    await expect(f.page.getByRole('dialog').getByRole('switch').last()).not.toBeChecked(); await fits();
    await f.page.getByRole('dialog').getByRole('button', { name: /^(Close|Schließen)$/ }).last().click();
    await f.page.getByRole('button', { name: /^(Draft an event assignment|Ereignisauftrag entwerfen)$/ }).click();
    await expect(f.page.locator('#assignment-name')).toHaveValue(/^(Respond to events|Auf Ereignisse reagieren)$/);
    await f.page.locator('#event-topic').click();
    await f.page.getByRole('option', { name: /Example event/ }).click();
    await f.page.getByRole('button', { name: /^(Event data|Ereignisdaten)$/ }).click();
    await expect(f.page.getByRole('dialog')).toContainText('recordId'); await fits();
    await f.page.getByRole('dialog').getByRole('button', { name: /^(Close|Schließen)$/ }).last().click();
    assert.equal((await connection.request('objects.query', { contractKey: 'secretary/assignment', where: { op: 'eq', field: 'object.parentId', value: root } })).items.length, 0);
    let firstAssignment, snapshot;
    for (let index = 0; index < 101; index++) {
      const assignment = { ...assignmentFor(at), assignmentId: randomUUID(), builtInKey: null, enabled: false, name: 'Assignment ' + index };
      const pin = await save('secretary/assignment', contractVersion('secretary/assignment'), 'assignment-' + index, assignment);
      if (index === 0) { firstAssignment = pin; snapshot = assignment; }
    }
    for (let index = 0; index < 51; index++) await save('secretary/execution', contractVersion('secretary/execution'), 'execution-' + index, {
      schemaVersion: 1, executionId: randomUUID(), assignment: firstAssignment, assignmentSnapshot: { ...snapshot, name: 'Execution ' + index },
      trigger: { kind: 'event', key: 'event:' + index, occurredAt: at, payload: {} }, effectiveRules: configuration.rules,
      executionTarget: { serviceNodeId: 'browser-agent', threadCwd: resolve('browser-secretary'), model: null, effort: 'medium', permissions: ':read-only' },
      phase: 'archived', serviceNodeId: 'browser-agent', nativeTarget: null, threadId: null, turnId: null,
      nativeOperations: { threadStart: null, turnStart: null, archive: null, delete: null }, preflightOutput: null, result: null, errorCode: null,
      createdAt: at, startedAt: null, completedAt: at, archivedAt: at, deleteAfter: null, deletedAt: null, updatedAt: at,
    });
    const requests = [];
    f.page.on('request', request => { if (request.method() === 'POST' && request.url().endsWith('/api/v1/rpc')) requests.push(request.postDataJSON()); });
    const pagination = f.page.getByLabel('Pagination'), rows = f.page.locator('main ol > li'), assignments = f.page.locator('main ul > li');
    await f.page.goto(f.base + '/ui/secretary-ui/#/journal');
    await expect(rows).toHaveCount(50); await fits();
    const beforeNext = requests.length;
    await pagination.getByRole('button', { name: 'Next', exact: true }).click();
    await expect(rows).toHaveCount(1); await expect(pagination).toContainText('Page 2'); await fits();
    assert.equal(requests.slice(beforeNext).filter(request => request.method === 'objects.query' && request.params.contractKey === 'secretary/execution').length, 1);
    await pagination.getByRole('button', { name: 'Previous', exact: true }).click(); await expect(rows).toHaveCount(50);
    await f.page.goto(f.base + '/ui/secretary-ui/#/assignments');
    await expect(assignments).toHaveCount(100); await fits();
    await assignments.first().getByRole('button').click();
    const notificationAge = f.page.locator('#notification-age'), saveAssignment = f.page.getByRole('dialog').getByRole('button', { name: /^(Save assignment|Auftrag speichern)$/ });
    await expect(notificationAge).toHaveValue('');
    await expect(f.page.getByRole('dialog')).toContainText(/service default|Dienststandard/);
    await notificationAge.fill('0'); await saveAssignment.click();
    await expect.poll(() => saves.length).toBe(1); assert.equal(saves[0].value.minimumNotificationAgeMinutes, 0);
    await notificationAge.fill(''); await saveAssignment.click();
    await expect.poll(() => saves.length).toBe(2); assert.equal(Object.hasOwn(saves[1].value, 'minimumNotificationAgeMinutes'), false);
    await fits(); await f.page.getByRole('dialog').getByRole('button', { name: /^(Close|Schließen)$/ }).last().click();
    await pagination.getByRole('button', { name: 'Next', exact: true }).click();
    await expect(assignments).toHaveCount(1); await expect(pagination).toContainText('Page 2'); await fits();
    const queries = requests.filter(request => request.method === 'objects.query' && request.params.contractKey.startsWith('secretary/'));
    assert.ok(queries.length >= 5); assert.ok(queries.every(request => request.params.includeContent === true));
    assert.equal(requests.filter(request => request.method === 'objects.read' && savedIds.has(request.params.objectId)).length, 0);
    assert.deepEqual(f.pageErrors, []); assert.deepEqual(f.externalRequests, []);
  });
}
