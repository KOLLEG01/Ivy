import { assignmentFor } from '../secretary-fixture.mjs';
import test from 'node:test';
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { resolve } from 'node:path';
import { randomUUID } from 'node:crypto';
import { gunzipSync } from 'node:zlib';
import { expect } from '@playwright/test';
import { ServiceClient, digest, deriveOperationId, IvyError, newOperationId } from '../../dist/packages/sdk/src/node.js';
import { publishUi } from '../../dist/packages/cli/src/publish-ui.js';
import { fixture, fixtureToken } from './fixtures/fixture.mjs';

// This UI transport fixture supplies canned read/save responses using the same
// fixed public registry corpus as the native registry contract cases.
const registryTemplate = JSON.parse(gunzipSync(await readFile(new URL('../secretary-rust/registry-cases.json.gz', import.meta.url)))).baseline;
const secretaryRegistry = () => structuredClone(registryTemplate);
const contractVersion = key => registryTemplate.contracts.find(definition => definition.key === key).version;

for (const viewport of [{ width: 1440, height: 900 }, { width: 390, height: 844 }]) {
  test(`Secretary loads scalar lists and retains original writes across reload at ${viewport.width}px`, { timeout: 150000 }, async t => {
    const f = await fixture(t, false, viewport);
    const agentStates = [];
    const backend = { backendId: 'reviewer', provider: 'codex', accountId: 'work' };
    const nativeStatus = JSON.parse(gunzipSync(await readFile(new URL('../secretary-rust/assignment-execution-cases.json.gz', import.meta.url)))).rows[71].input.steps[1].value;
    let advertisedBackends = [{ ...nativeStatus, serviceNodeId: 'browser-agent', hostId: 'fixture', backend }];
    const agent = new ServiceClient({ publicBaseUrl: f.base, credential: () => fixtureToken,
      identity: { serviceNodeId: 'browser-agent', serviceName: 'agent-manager', hostId: 'fixture', version: 'test', buildId: digest('browser-agent'), hiveProtocol: 1 },
      registry: () => ({ namespaces: [{ namespace: 'agent', description: 'Execution provider fixture', guideMarkdown: '', topics: [], inventoryKinds: [],
        tools: [{ namespace: 'agent', name: 'status', interfaceVersion: '1.0.0', description: 'Read execution provider readiness.',
          inputSchema: { type: 'object', additionalProperties: false }, outputSchema: { type: 'object' }, annotations: { readOnlyHint: true } },
        { namespace: 'agent', name: 'backends', interfaceVersion: '1.0.0', description: 'Read offered accounts.',
          inputSchema: { type: 'object', additionalProperties: false }, outputSchema: { type: 'object' }, annotations: { readOnlyHint: true } }] }], contracts: [], requiredContracts: [] }),
      handlers: { 'agent.status': () => ({}), 'agent.backends': () => ({ backends: advertisedBackends }) }, reconcile: async () => {}, onState: state => agentStates.push(state) });
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
    const receipts = new Map();
    let hideReceipt = false;
    const configurationSaves = [];
    let connection;
    let assignmentResponse = null, configurationResponse = null, acceptAssignments = false;
    handlers['secretary.saveAssignment'] = async args => {
      saves.push(structuredClone(args));
      if (!acceptAssignments) throw new IvyError('fixture_capture_only', 'Save captured without changing the fixture.', 'not_executed');
      const saved = await connection.request('objects.write', { mutationId: deriveOperationId(args.operationId, ['browser-fixture-save-assignment']),
        contractVersion: contractVersion('secretary/assignment'), references: {}, objectId: args.assignment.objectId,
        expectedRevision: args.assignment.revision, content: { encoding: 'json', value: args.value } });
      const pin = { objectId: saved.object.id, revision: saved.revision.revision };
      receipts.set(args.operationId, { operationId: args.operationId, action: 'update_assignment', phase: 'succeeded', effect: pin, errorCode: null, observedAt: at });
      if (assignmentResponse) { const response = assignmentResponse; assignmentResponse = null; await response; }
      return pin;
    };
    handlers['secretary.saveConfiguration'] = async args => {
      configurationSaves.push(structuredClone(args));
      const saved = await connection.request('objects.write', { mutationId: deriveOperationId(args.operationId, ['browser-fixture-save-configuration']), contractVersion: contractVersion('secretary/configuration'), references: {},
        objectId: args.configuration.objectId, expectedRevision: args.configuration.revision, content: { encoding: 'json', value: args.value } });
      const pin = { objectId: saved.object.id, revision: saved.revision.revision };
      receipts.set(args.operationId, { operationId: args.operationId, action: 'configure_execution', phase: 'succeeded', effect: pin, errorCode: null, observedAt: at });
      if (configurationResponse) { const response = configurationResponse; configurationResponse = null; await response; }
      return pin;
    };
    handlers['secretary.operationRead'] = args => {
      if (hideReceipt || !receipts.has(args.operationId)) throw new IvyError('not_found', 'No retained original receipt.');
      return receipts.get(args.operationId);
    };
    handlers['secretary.binding'] = (_args, context) => ({ serviceNodeId: 'browser-secretary', hostId: 'fixture', available: true, expectedScope: scope,
      generation: context.generation, bindingHash: digest('browser-secretary-binding'), observedAt: at });
    const secretaryStates = [];
    const secretary = new ServiceClient({ publicBaseUrl: f.base, credential: () => fixtureToken,
      identity: { serviceNodeId: 'browser-secretary', serviceName: 'secretary', hostId: 'fixture', version: 'test', buildId: digest('browser-secretary'), hiveProtocol: 1 },
      registry: () => registry, handlers, reconcile: async () => {}, onState: state => secretaryStates.push(state) });
    t.after(() => secretary.stop()); secretary.start();
    connection = await secretary.waitReady({ timeoutMs: 10000 }).catch(cause => { throw new Error(JSON.stringify(secretaryStates.slice(-6)), { cause }); });
    const savedIds = new Set();
    const save = async (contractKey, contractVersion, name, value) => {
      const saved = await connection.request('objects.write', { mutationId: await newOperationId(connection), contractVersion, references: {},
        create: { contractKey, parentId: root, ownerObjectId: null, name }, content: { encoding: 'json', value } });
      savedIds.add(saved.object.id); return { objectId: saved.object.id, revision: saved.revision.revision };
    };
    const configuration = { schemaVersion: 1, execution: null, browserNotifications: true, updatedAt: at, rules: {
      main: { enabled: true, minimumUrgency: 'normal', window: null },
      whatsapp: { enabled: true, minimumUrgency: 'normal', window: null, questionsOnly: false },
      voice: { enabled: true, minimumUrgency: 'critical', window: null, immediateOnly: true }, researchMaxMinutes: 5 } };
    await save('secretary/configuration', contractVersion('secretary/configuration'), 'configuration', configuration);
    await publishUi(f.client, { directory: resolve('dist/apps/secretary-ui'), definition: JSON.parse(await readFile('ui/secretary-ui/ui.json', 'utf8')),
      expectedReleaseId: null, mutationId: await newOperationId(f.client) });
    await f.login();
    const fits = async () => assert.ok(await f.page.evaluate(() => document.documentElement.scrollWidth <= innerWidth + 2), 'The Secretary page must fit its viewport.');
    const requests = [];
    f.page.on('request', request => { if (request.method() === 'POST' && request.url().endsWith('/api/v1/rpc')) requests.push(request.postDataJSON()); });
    await f.page.goto(f.base + '/ui/secretary-ui/#/journal');
    await f.page.getByRole('link', { name: /Explore what Secretary can do|Möglichkeiten ansehen/ }).waitFor({ timeout: 5000 })
      .catch(async cause => { throw new Error(await f.page.locator('body').innerText(), { cause }); });
    await f.page.getByRole('link', { name: /Explore what Secretary can do|Möglichkeiten ansehen/ }).click();
    assert.equal(requests.filter(request => request.method === 'topics.list').length, 0, 'Topics stay unloaded on the list');
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
    assert.equal(requests.filter(request => request.method === 'topics.list').length, 1, 'Opening an event editor loads the topics once');
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
      nativeOperations: { threadStart: null, turnStart: null, archive: null, delete: null }, preflightOutput: null, result: 'retained large result ' + 'x'.repeat(100000), errorCode: null,
      createdAt: at, startedAt: null, completedAt: at, archivedAt: at, deleteAfter: null, deletedAt: null, updatedAt: at,
    });
    const pagination = f.page.getByLabel('Pagination'), rows = f.page.locator('main ol > li'), assignments = f.page.locator('main ul > li');
    await f.page.evaluate(root => {
      for (const prefix of ['secretary-assignments:', 'secretary-journal:']) sessionStorage.setItem(prefix + root, JSON.stringify({ cursors: [null, 'previous-full-query-cursor'], page: 2 }));
    }, root);
    await f.page.goto(f.base + '/ui/secretary-ui/#/journal');
    await expect(rows).toHaveCount(50); await fits();
    assert.equal(requests.filter(request => request.method === 'objects.read' && savedIds.has(request.params.objectId)).length, 0);
    const executionQueries = requests.filter(request => request.method === 'objects.query' && request.params.contractKey === 'secretary/execution');
    assert.ok(executionQueries.every(request => request.params.includeContent === false && !request.params.select.includes('data:/result')));
    await rows.first().getByRole('button', { name: 'View details' }).click();
    await expect(f.page.getByRole('dialog')).toContainText('retained large result');
    const detailReads = requests.filter(request => request.method === 'objects.read' && savedIds.has(request.params.objectId));
    assert.equal(detailReads.length, 1); assert.equal(typeof detailReads[0].params.revision, 'number');
    await fits(); await f.page.getByRole('dialog').getByRole('button', { name: /^(Close|Schließen)$/ }).last().click();
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
    for (const key of ['secretary/execution', 'secretary/assignment'])
      assert.ok(queries.some(request => request.params.contractKey === key), 'Pagination queries documents for ' + key);
    assert.ok(queries.filter(request => request.params.contractKey !== 'secretary/configuration').every(request => request.params.includeContent === false));
    assert.equal(requests.filter(request => request.method === 'objects.read' && savedIds.has(request.params.objectId)).length, 2, 'One execution detail and one assignment editor read exact bodies');
    await f.page.goto(f.base + '/ui/secretary-ui/#/rules');
    const globalQuietHours = { start: '23:00', end: '07:00', timeZone: 'Europe/Berlin' };
    await f.client.request('settings.save', { mutationId: await newOperationId(f.client), expectedRevision: 0,
      settings: { schemaVersion: 1, quietHours: globalQuietHours } });
    await expect(f.page.getByText('23:00–07:00 · Europe/Berlin', { exact: true })).toBeVisible();
    await expect(f.page.getByRole('link', { name: /^(Open Hive settings|Hive-Einstellungen öffnen)$/ })).toHaveAttribute('href', f.base + '/#/system/settings');
    const desktopNotifications = f.page.getByRole('switch', { name: /^(Allow desktop notifications|Desktop-Benachrichtigungen erlauben)$/ });
    const saveSettings = f.page.getByRole('button', { name: /^(Save|Speichern)$/ });
    await expect(desktopNotifications).toBeChecked(); await fits();
    await desktopNotifications.click(); await saveSettings.click();
    await expect(f.page.getByRole('status')).toContainText(/Saved revision|Revision .* gespeichert/);
    assert.equal(configurationSaves.length, 1); assert.equal(configurationSaves[0].value.browserNotifications, false);
    await f.page.reload(); await expect(desktopNotifications).not.toBeChecked(); await fits();
    await desktopNotifications.click(); await saveSettings.click();
    await expect(f.page.getByRole('status')).toContainText(/Saved revision|Revision .* gespeichert/);
    assert.equal(configurationSaves.length, 2); assert.equal(configurationSaves[1].value.browserNotifications, true);
    assert.deepEqual((await f.client.request('settings.get', {})).settings.quietHours, globalQuietHours);
    await f.page.reload(); await expect(desktopNotifications).toBeChecked(); await fits();
    await f.page.getByRole('button', { name: /Configure target|Ziel konfigurieren/ }).click();
    await f.page.locator('#execution-manager').click();
    await f.page.getByRole('option', { name: 'fixture', exact: true }).click();
    await f.page.locator('#execution-backend').click();
    await f.page.getByRole('option', { name: 'Codex · work (reviewer)', exact: true }).click();
    await f.page.locator('#execution-cwd').fill('/workspace');
    await saveSettings.click();
    await expect(f.page.getByRole('status')).toContainText(/Saved revision|Revision .* gespeichert/);
    assert.deepEqual(configurationSaves.at(-1).value.execution.backend, backend);
    await f.page.reload();
    await expect(f.page.locator('#execution-backend')).toHaveAttribute('data-value', JSON.stringify(['reviewer', 'codex', 'work']));
    advertisedBackends = [...advertisedBackends, { ...advertisedBackends[0], backend: { backendId: 'personal', provider: 'codex', accountId: 'personal' } }];
    await f.page.reload();
    await f.page.locator('#execution-backend').click();
    await f.page.getByRole('option', { name: 'Codex · personal (personal)', exact: true }).click();
    await expect(f.page.locator('#execution-cwd')).toHaveValue('');
    await f.page.locator('#execution-cwd').fill('/personal');
    let finishConfiguration;
    configurationResponse = new Promise(resolve => { finishConfiguration = resolve; });
    const configurationCount = configurationSaves.length;
    await saveSettings.click();
    await expect.poll(() => configurationSaves.length).toBe(configurationCount + 1);
    await f.page.locator('#execution-cwd').fill('/edited-during-save');
    finishConfiguration();
    await expect(f.page.getByRole('status')).toContainText(/Saved revision|Revision .* gespeichert/);
    await expect(f.page.locator('#execution-cwd')).toHaveValue('/edited-during-save');
    await f.page.reload();
    await expect(f.page.locator('#execution-backend')).toHaveAttribute('data-value', JSON.stringify(['personal', 'codex', 'personal']));
    await expect(f.page.locator('#execution-cwd')).toHaveValue('/personal');
    await fits();
    await f.page.goto(f.base + '/ui/secretary-ui/#/assignments');
    await expect(assignments).toHaveCount(1);
    await pagination.getByRole('button', { name: 'Previous', exact: true }).click();
    await expect(assignments).toHaveCount(100);
    acceptAssignments = true;
    let finishAssignment;
    assignmentResponse = new Promise(resolve => { finishAssignment = resolve; });
    const saveCount = saves.length;
    await assignments.nth(0).getByRole('switch').click();
    await expect.poll(() => saves.length).toBe(saveCount + 1);
    await expect(assignments.nth(1).getByRole('switch')).toBeEnabled();
    await assignments.nth(1).getByRole('switch').click();
    await expect.poll(() => saves.length).toBe(saveCount + 2);
    finishAssignment();
    await fits();
    await expect.poll(() => receipts.size).toBeGreaterThan(0);
    await f.page.reload(); await expect(assignments).toHaveCount(100);
    let finishReload;
    assignmentResponse = new Promise(resolve => { finishReload = resolve; });
    const reloadCount = saves.length;
    await assignments.nth(2).getByRole('switch').click();
    await expect.poll(() => saves.length).toBe(reloadCount + 1);
    await expect.poll(() => receipts.has(saves.at(-1).operationId)).toBe(true);
    const originalToggle = structuredClone(saves.at(-1));
    await f.page.reload();
    const check = f.page.getByRole('button', { name: 'Check again', exact: true });
    await expect(check).toHaveCount(1);
    await check.click();
    await expect(check).toHaveCount(0);
    assert.equal(saves.length, reloadCount + 1, 'A confirmed original is read, not rewritten after reload');
    finishReload();
    await f.page.goto(f.base + '/ui/secretary-ui/#/rules');
    await expect(f.page.getByRole('button', { name: 'Save', exact: true })).toBeEnabled();
    let finishReloadConfiguration;
    configurationResponse = new Promise(resolve => { finishReloadConfiguration = resolve; });
    const reloadConfigurationCount = configurationSaves.length;
    await desktopNotifications.click(); await saveSettings.click();
    await expect.poll(() => configurationSaves.length).toBe(reloadConfigurationCount + 1);
    await expect.poll(() => receipts.has(configurationSaves.at(-1).operationId)).toBe(true);
    const originalConfiguration = structuredClone(configurationSaves.at(-1));
    await f.page.reload(); await expect(check).toHaveCount(1);
    const retainedKey = await f.page.evaluate(() => Object.keys(sessionStorage).find(key => key.startsWith('ivy:secretary:writes:')));
    assert.ok(retainedKey);
    const currentStatus = await f.client.request('system.status', {});
    const changeCaller = async route => {
      const body = route.request().postDataJSON();
      if (body.method !== 'system.status') return route.continue();
      await route.fulfill({ status: 200, contentType: 'application/json', json: { jsonrpc: '2.0', id: body.id, result: { ...currentStatus, callerPrincipalId: 'another-caller' } } });
    };
    await f.page.route('**/api/v1/rpc', changeCaller);
    await check.click();
    await expect(f.page.getByRole('alert')).toContainText('original caller');
    assert.equal(configurationSaves.length, reloadConfigurationCount + 1);
    await f.page.unroute('**/api/v1/rpc', changeCaller);
    // A missing receipt still uses the exact original operation, pin and payload.
    hideReceipt = true;
    await check.click();
    await expect(f.page.getByRole('status')).toContainText('Saved revision');
    assert.deepEqual(configurationSaves.at(-1), originalConfiguration);
    assert.equal(originalToggle.value.enabled, true);
    hideReceipt = false; finishReloadConfiguration();
    // Browser persistence failure prevents a new mutation, without disabling reads.
    const beforeBlockedSave = configurationSaves.length;
    await f.page.evaluate(key => sessionStorage.setItem(key, 'invalid'), retainedKey);
    await saveSettings.click();
    await expect(f.page.getByRole('alert')).toBeVisible();
    assert.equal(configurationSaves.length, beforeBlockedSave);
    await fits();
    assert.ok(requests.every(request => request.params?.cursor !== 'previous-full-query-cursor'), 'A saved position belongs to its exact list projection');
    assert.deepEqual(f.pageErrors, []); assert.deepEqual(f.externalRequests, []);
  });
}
