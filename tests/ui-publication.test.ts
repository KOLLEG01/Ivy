import test from 'node:test';
import type { TestContext } from 'node:test';
import assert from 'node:assert/strict';
import { existsSync, mkdtempSync, mkdirSync, rmSync, symlinkSync, utimesSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, relative } from 'node:path';
import { randomUUID } from 'node:crypto';
import { HiveKernel } from '../services/hive/src/kernel.js';
import { digest } from '../packages/contracts/src/canonical.js';
import { IvyError } from '../packages/contracts/src/errors.js';
import type { Operation, OperationName, Params, Result } from '../packages/contracts/src/generated.js';
import type { RpcClient } from '../packages/sdk/src/client.js';
import { publishUi, readUiBundle } from '../packages/cli/src/publish-ui.js';
import wikiDefinition from '../ui/wiki-ui/ui.json' with { type: 'json' };
import { operationId } from '../packages/contracts/src/operation-id.js';

function fixture(t: TestContext) {
  const root = mkdtempSync(join(tmpdir(), 'ivy-ui-publication-')), bundle = join(root, 'bundle'); mkdirSync(bundle);
  writeFileSync(join(bundle, 'index.html'), '<!doctype html><script type="module" src="./main.js"></script>');
  writeFileSync(join(bundle, 'main.js'), '\uFEFFdocument.body.textContent = "r1";');
  writeFileSync(join(bundle, 'LICENSE.txt'), 'Synthetic test notice.');
  const credential = { principalId: 'publication-fixture', digest: digest('synthetic-publication') };
  const kernel = new HiveKernel({ filename: ':memory:', publicBaseUrl: 'https://fixture.test/ivy', version: 'fixture', buildId: digest('fixture'), credentials: [credential] });
  t.after(() => { kernel.close(); assert.ok(relative(tmpdir(), root).startsWith('ivy-ui-publication-')); rmSync(root, { recursive: true, force: true }); });
  const client: RpcClient = { async request<M extends OperationName>(method: M, params: Params<M>): Promise<Result<M>> {
    const result = kernel.execute({ credentialDigest: credential.digest, principalId: credential.principalId, transport: 'http' }, { jsonrpc: '2.0', method, params, id: randomUUID() });
    if (result.kind !== 'result') throw new Error('Unexpected native dispatch.'); return result.value as Result<M>;
  } };
  const mutation = (nonce: string) => operationId(kernel.store.runtimeEpoch, Date.now(), nonce);
  const input = { directory: bundle, definition: wikiDefinition as Operation.UiDefinition, expectedReleaseId: null, mutationId: mutation('original-publication') };
  return { root, bundle, kernel, client, input, mutation };
}
const code = (expected: string) => (error: unknown) => error instanceof IvyError && error.code === expected;

test('UI publication rejects unreadable retained data before promoting files or changing the active release', async t => {
  const { bundle, kernel, client, input, mutation } = fixture(t);
  const key = 'fixture/journal';
  for (const version of ['1.0.0', '1.5.0']) await client.request('contracts.register', {
    mutationId: mutation('register-' + version), definition: { key, version, owner: { kind: 'agent' }, mediaType: 'text/plain',
      retention: { objects: { mode: 'retain' }, revisions: { mode: 'all' } }, specMarkdown: 'Isolated journal version fixture.' },
  });
  await client.request('objects.write', { mutationId: mutation('journal-current'), contractVersion: '1.5.0', references: {},
    create: { contractKey: key, name: 'Journal entry', parentId: null, ownerObjectId: null }, content: { encoding: 'text', value: 'Retained journal' } });
  const definition: Operation.UiDefinition = { ...input.definition, dataContracts: [], requirements: { hiveProtocol: 1, services: [],
    contracts: [{ key, readVersions: ['1.0.0', '1.5.0'], writeVersions: [] }] } };
  const active = await publishUi(client, { ...input, definition });
  writeFileSync(join(bundle, 'main.js'), 'document.body.textContent = "incompatible journal";');
  const incompatible = { ...definition, requirements: { ...definition.requirements,
    contracts: [{ key, readVersions: ['1.0.0'], writeVersions: [] }] } };
  const candidate = await readUiBundle(bundle, incompatible);
  await assert.rejects(publishUi(client, { ...input, definition: incompatible, expectedReleaseId: active.releaseId,
    mutationId: mutation('incompatible-publication') }), code('contract_version_conflict'));
  assert.equal((await client.request('uis.get', { uiId: definition.metadata.uiId })).currentReleaseId, active.releaseId);
  assert.equal((await client.request('uis.inspect', { uiId: definition.metadata.uiId })).status, 'ready');
  assert.equal(existsSync(kernel.uiFiles.directory(definition.metadata.uiId, candidate.releaseId)), false);
});

test('UI metadata orders catalog pages by priority and admits only unique unreserved short paths', async t => {
  const { kernel, client, input, mutation } = fixture(t);
  const definition = (uiId: string, priority?: number, slug?: string): Operation.UiDefinition => ({
    ...input.definition, dataContracts: [],
    metadata: { uiId, displayName: uiId, description: 'Navigation fixture', iconKey: 'ui',
      ...(priority === undefined ? {} : { priority }), ...(slug ? { slug } : {}) },
  });
  for (const item of [definition('z-wiki', 600, 'wiki'), definition('a-board', 500, 'taskboard'), definition('c-default'), definition('b-default'), definition('negative', -1)]) {
    await publishUi(client, { ...input, definition: item, mutationId: mutation(item.metadata.uiId) });
  }
  const ids: string[] = []; let cursor: string | null = null;
  do {
    const page: Operation.UisCatalogResult = await client.request('uis.catalog', { limit: 1, ...(cursor ? { cursor } : {}) });
    ids.push(...page.items.map(ui => ui.metadata.uiId)); cursor = page.nextCursor;
  } while (cursor);
  assert.deepEqual(ids, ['z-wiki', 'a-board', 'b-default', 'c-default', 'negative']);
  assert.equal(kernel.uis.resolveSlug('wiki'), 'z-wiki');
  assert.equal(kernel.uis.resolveSlug('unknown'), null);
  await assert.rejects(publishUi(client, { ...input, definition: definition('duplicate', 0, 'wiki'), mutationId: mutation('duplicate') }), code('invalid_arguments'));
  for (const slug of ['api', 'mcp', 'mcp-dev', 'ui', 'console', 'oauth', 'login', 'logout', 'health', '../wiki', 'wiki/subpage']) {
    await assert.rejects(readUiBundle(input.directory, definition('reserved', 0, slug)), code('invalid_arguments'));
  }
});

test('publication stages private files, recovers a lost response and fences a changed release pointer', async t => {
  const { bundle, kernel, client, input } = fixture(t); let writes = 0;
  const dropping: RpcClient = { async request<M extends OperationName>(method: M, params: Params<M>): Promise<Result<M>> {
    const result = await client.request(method, params);
    if (method === 'uis.stageAsset' && ++writes === 2) throw new IvyError('outcome_unknown', 'Synthetic lost asset response.', 'unknown');
    return result;
  } };
  await assert.rejects(publishUi(dropping, input), code('outcome_unknown'));
  assert.equal((await client.request('uis.catalog', {})).items.length, 0);
  const result = await publishUi(client, input);
  assert.equal((await client.request('uis.inspect', { uiId: 'wiki-ui' })).status, 'ready');
  const bom = kernel.uis.asset('wiki-ui', result.releaseId, 'main.js');
  assert.equal(bom.asset.contentHash, digest('\uFEFFdocument.body.textContent = "r1";'));
  assert.equal((await kernel.uiFiles.read('wiki-ui', result.releaseId, bom.asset)).toString(), '\uFEFFdocument.body.textContent = "r1";');
  assert.deepEqual(await publishUi(client, input), result);
  const count = kernel.store.get('SELECT COUNT(*) AS count FROM objects')!['count'];
  writeFileSync(join(bundle, 'main.js'), 'different build');
  await assert.rejects(publishUi(client, input), code('mutation_conflict'));
  assert.equal(kernel.store.get('SELECT COUNT(*) AS count FROM objects')!['count'], count);
  assert.equal(kernel.store.get('SELECT COUNT(*) AS count FROM app_assets')!['count'], 0);
});

test('a lost final publication response replays its original result after a later release without moving the pointer back', async t => {
  const { bundle, client, input, mutation } = fixture(t); let lost = true;
  const dropping: RpcClient = { async request<M extends OperationName>(method: M, params: Params<M>): Promise<Result<M>> {
    const result = await client.request(method, params);
    if (method === 'uis.deploy' && lost) { lost = false; throw new IvyError('outcome_unknown', 'Synthetic lost pointer response.', 'unknown'); }
    return result;
  } };
  await assert.rejects(publishUi(dropping, input), code('outcome_unknown'));
  const original = (await client.request('uis.get', { uiId: 'wiki-ui' })).currentReleaseId!;
  writeFileSync(join(bundle, 'main.js'), 'document.body.textContent = "r2";');
  const next = await publishUi(client, { ...input, mutationId: mutation('next-publication'), expectedReleaseId: original });
  writeFileSync(join(bundle, 'main.js'), '\uFEFFdocument.body.textContent = "r1";');
  const recovered = await publishUi(client, input); assert.equal(recovered.releaseId, original); assert.equal(recovered.previousReleaseId, null);
  assert.equal((await client.request('uis.get', { uiId: 'wiki-ui' })).currentReleaseId, next.releaseId);
});

test('collection removes old private release directories while retaining current and previous', async t => {
  const { bundle, kernel, client, input, mutation } = fixture(t);
  const first = await publishUi(client, input);
  writeFileSync(join(bundle, 'main.js'), 'document.body.textContent = "r2";');
  const second = await publishUi(client, { ...input, expectedReleaseId: first.releaseId, mutationId: mutation('r2') });
  writeFileSync(join(bundle, 'main.js'), 'document.body.textContent = "r3";');
  const third = await publishUi(client, { ...input, expectedReleaseId: second.releaseId, mutationId: mutation('r3') });
  assert.deepEqual((await client.request('uis.get', { uiId: 'wiki-ui' })).releases.map(value => value.releaseId).sort(),
    [second.releaseId, third.releaseId].sort());
  const old = kernel.uiFiles.directory('wiki-ui', first.releaseId);
  const aged = new Date(Date.now() - 25 * 60 * 60 * 1000);
  utimesSync(old, aged, aged);
  kernel.uis.collectFiles();
  assert.equal(existsSync(old), false);
  assert.equal(existsSync(kernel.uiFiles.directory('wiki-ui', second.releaseId)), true);
  assert.equal(existsSync(kernel.uiFiles.directory('wiki-ui', third.releaseId)), true);
  const abandoned = kernel.uiFiles.directory('abandoned-ui', 'stale-release');
  mkdirSync(abandoned, { recursive: true });
  writeFileSync(join(abandoned, 'index.html'), 'abandoned');
  utimesSync(abandoned, aged, aged);
  const staged = join(kernel.uiFiles.root, '.staging', 'abandoned-ui', 'stale-release');
  mkdirSync(staged, { recursive: true });
  utimesSync(staged, aged, aged);
  kernel.uis.collectFiles();
  assert.equal(existsSync(join(kernel.uiFiles.root, 'abandoned-ui')), false);
  assert.equal(existsSync(join(kernel.uiFiles.root, '.staging', 'abandoned-ui')), false);
});

test('publication refuses outside links and unsupported members before registering any ui data', async t => {
  const { root, bundle, client, input } = fixture(t);
  const contracts = (await client.request('contracts.list', {})).items;
  const outside = join(root, 'outside'); mkdirSync(outside); writeFileSync(join(outside, 'private.txt'), 'Private fixture only.');
  symlinkSync(outside, join(bundle, 'linked'), process.platform === 'win32' ? 'junction' : 'dir');
  await assert.rejects(readUiBundle(bundle, input.definition), code('invalid_arguments'));
  await assert.rejects(publishUi(client, input), code('invalid_arguments'));
  assert.deepEqual((await client.request('contracts.list', {})).items, contracts);
  const unsupported = join(root, 'unsupported'); mkdirSync(unsupported);
  writeFileSync(join(unsupported, 'index.html'), '<h1>Fixture</h1>'); writeFileSync(join(unsupported, 'unexpected.exe'), 'Never executed.');
  await assert.rejects(readUiBundle(unsupported, input.definition), code('invalid_arguments'));
});

test('an obsolete persisted app row remains inspectable and cannot block a current package replacement', async t => {
  const { bundle, kernel, client, input, mutation } = fixture(t);
  const original = await publishUi(client, input);
  const row = kernel.store.get('SELECT release_json FROM app_releases WHERE app_id=? AND release_id=?', 'wiki-ui', original.releaseId)!;
  const obsolete = JSON.parse(String(row['release_json'])) as Record<string, unknown>;
  obsolete['requirements'] = { hiveProtocol: 1, contracts: [], services: [{ serviceNodeId: 'HOST-B.agent-manager', namespace: 'agent', interfaceVersion: '1.0.0' }] };
  kernel.store.run('UPDATE app_releases SET release_json=? WHERE app_id=? AND release_id=?', JSON.stringify(obsolete), 'wiki-ui', original.releaseId);
  kernel.store.run('UPDATE apps SET metadata=? WHERE app_id=?', JSON.stringify({ appId: 'wiki-ui', displayName: 'Wiki', description: 'Obsolete fixture', iconKey: 'book-open' }), 'wiki-ui');

  const inspection = await client.request('uis.inspect', { uiId: 'wiki-ui' });
  assert.equal(inspection.status, 'invalid'); assert.equal(inspection.currentReleaseId, original.releaseId); assert.equal(inspection.requirements, null);

  writeFileSync(join(bundle, 'main.js'), 'document.body.textContent = "current";');
  const replacement = await publishUi(client, { ...input, mutationId: mutation('current-contract-replacement'), expectedReleaseId: original.releaseId });
  assert.equal((await client.request('uis.get', { uiId: 'wiki-ui' })).currentReleaseId, replacement.releaseId);
  assert.equal((await client.request('uis.inspect', { uiId: 'wiki-ui' })).status, 'ready');
});

test('existing object-backed releases move to private files and leave the object index', async t => {
  const root = mkdtempSync(join(tmpdir(), 'ivy-ui-upgrade-'));
  const filename = join(root, 'hive.sqlite');
  const credential = { principalId: 'upgrade-fixture', digest: digest('upgrade-fixture') };
  const options = { filename, publicBaseUrl: 'https://fixture.test/ivy', version: 'fixture',
    buildId: digest('upgrade-fixture'), credentials: [credential] };
  let kernel = new HiveKernel(options);
  t.after(() => { kernel.close(); rmSync(root, { recursive: true, force: true }); });
  const context = { credentialDigest: credential.digest, principalId: credential.principalId, transport: 'http' as const };
  const request = (method: string, params: unknown) => {
    const result = kernel.execute(context, { jsonrpc: '2.0', id: randomUUID(), method, params });
    if (result.kind !== 'result') throw new Error('Unexpected dispatch.');
    return result.value;
  };
  request('contracts.register', { mutationId: operationId(kernel.store.runtimeEpoch, Date.now(), 'legacy-contract'),
    definition: { key: 'ui/asset-legacy', version: '1.0.0', owner: { kind: 'agent' },
      mediaType: 'text/html', retention: { objects: { mode: 'retain' }, revisions: { mode: 'current' } },
      specMarkdown: 'Legacy UI file.' } });
  const content = '<!doctype html><p>retained release</p>';
  const saved = request('objects.write', { mutationId: operationId(kernel.store.runtimeEpoch, Date.now(), 'legacy-file'),
    contractVersion: '1.0.0', references: {},
    create: { contractKey: 'ui/asset-legacy', name: 'UI asset legacy', parentId: null, ownerObjectId: null },
    content: { encoding: 'text', value: content } }) as Operation.ObjectWriteResult;
  const oldRelease = { releaseId: 'r1', entryPath: 'index.html',
    requirements: { hiveProtocol: 1, contracts: [], services: [] },
    assets: [{ path: 'index.html', objectId: saved.object.id, revision: 1,
      mediaType: 'text/html', contentHash: saved.revision.contentHash }] };
  kernel.store.run('INSERT INTO apps VALUES (?,?,?,NULL)', 'upgrade-ui',
    JSON.stringify({ uiId: 'upgrade-ui', displayName: 'Upgrade', description: 'Fixture', iconKey: 'ui' }), 'r1');
  kernel.store.run('INSERT INTO app_releases VALUES (?,?,?)', 'upgrade-ui', 'r1', JSON.stringify(oldRelease));
  kernel.store.run('INSERT INTO app_assets VALUES (?,?,?,?,?,?,?)',
    'upgrade-ui', 'r1', 'index.html', saved.object.id, 1, 'text/html', saved.revision.contentHash);
  kernel.store.run("DELETE FROM metadata WHERE key='ui_files_v1'");
  const interrupted = join(root, 'ui-releases', 'upgrade-ui', 'r1',
    'index.html.00000000-0000-4000-8000-000000000001.partial');
  mkdirSync(join(root, 'ui-releases', 'upgrade-ui', 'r1'), { recursive: true });
  writeFileSync(interrupted, 'interrupted migration');
  kernel.close();
  kernel = new HiveKernel(options);
  const selected = kernel.uis.asset('upgrade-ui', 'r1', 'index.html');
  assert.equal((await kernel.uiFiles.read('upgrade-ui', 'r1', selected.asset)).toString(), content);
  assert.equal(kernel.uis.inspect({ uiId: 'upgrade-ui' }).status, 'ready');
  assert.equal(kernel.store.get('SELECT COUNT(*) AS count FROM app_assets')!['count'], 0);
  assert.equal(kernel.store.get("SELECT COUNT(*) AS count FROM objects WHERE contract_key LIKE 'ui/asset-%'")!['count'], 0);
  assert.equal(kernel.store.get("SELECT COUNT(*) AS count FROM object_fts WHERE name MATCH 'legacy'")!['count'], 0);
  assert.equal(existsSync(interrupted), false);
});
