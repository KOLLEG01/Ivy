import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, mkdirSync, rmSync, symlinkSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { IvyError } from '../dist/packages/contracts/src/errors.js';
import { HiveClient, newOperationId } from '../dist/packages/sdk/src/client.js';
import { publishUi, readUiBundle } from '../dist/packages/cli/src/publish-ui.js';
import { startTestHive } from '../tools/testing/native-service.mjs';
import wikiDefinition from '../ui/wiki-ui/ui.json' with { type: 'json' };

async function fixture(t) {
  const root = mkdtempSync(join(tmpdir(), 'ivy-ui-publication-')), bundle = join(root, 'bundle');
  mkdirSync(bundle);
  writeFileSync(join(bundle, 'index.html'), '<!doctype html><script type="module" src="./main.js"></script>');
  writeFileSync(join(bundle, 'main.js'), '\uFEFFdocument.body.textContent = "r1";');
  writeFileSync(join(bundle, 'LICENSE.txt'), 'Synthetic test notice.');
  let hive;
  t.after(async () => { await hive?.close(); rmSync(root, { recursive: true, force: true }); });
  const token = 'synthetic-publication';
  hive = await startTestHive({ root: join(root, 'hive'), credentials: [{ principalId: 'publication-fixture', token }] });
  const client = new HiveClient(hive.config.publicBaseUrl, { credential: token });
  const input = { directory: bundle, definition: wikiDefinition,
    expectedReleaseId: null, mutationId: await newOperationId(client) };
  return { root, bundle, client, input };
}
const code = (expected) => (error) => error instanceof IvyError && error.code === expected;

test('publication recovers a lost staged asset response and fences changed bytes', async t => {
  const { bundle, client, input } = await fixture(t); let writes = 0;
  const dropping = { async request(method, params) {
    const result = await client.request(method, params);
    if (method === 'uis.stageAsset' && ++writes === 2) throw new IvyError('outcome_unknown', 'Synthetic lost asset response.', 'unknown');
    return result;
  } };
  await assert.rejects(publishUi(dropping, input), code('outcome_unknown'));
  assert.equal((await client.request('uis.catalog', {})).items.length, 0);
  const result = await publishUi(client, input);
  assert.equal((await client.request('uis.inspect', { uiId: 'wiki-ui' })).status, 'ready');
  assert.deepEqual(await publishUi(client, input), result);
  writeFileSync(join(bundle, 'main.js'), 'different build');
  await assert.rejects(publishUi(client, input), code('mutation_conflict'));
  assert.equal((await client.request('uis.get', { uiId: 'wiki-ui' })).currentReleaseId, result.releaseId);
});

test('a lost final publication response replays after a later release without moving its pointer back', async t => {
  const { bundle, client, input } = await fixture(t); let lost = true;
  const dropping = { async request(method, params) {
    const result = await client.request(method, params);
    if (method === 'uis.deploy' && lost) { lost = false; throw new IvyError('outcome_unknown', 'Synthetic lost pointer response.', 'unknown'); }
    return result;
  } };
  await assert.rejects(publishUi(dropping, input), code('outcome_unknown'));
  const original = (await client.request('uis.get', { uiId: 'wiki-ui' })).currentReleaseId;
  writeFileSync(join(bundle, 'main.js'), 'document.body.textContent = "r2";');
  const next = await publishUi(client, { ...input, mutationId: await newOperationId(client), expectedReleaseId: original });
  writeFileSync(join(bundle, 'main.js'), '\uFEFFdocument.body.textContent = "r1";');
  const recovered = await publishUi(client, input);
  assert.equal(recovered.releaseId, original); assert.equal(recovered.previousReleaseId, null);
  assert.equal((await client.request('uis.get', { uiId: 'wiki-ui' })).currentReleaseId, next.releaseId);
});

test('publication refuses outside links and unsupported members before registering UI data', async t => {
  const { root, bundle, client, input } = await fixture(t);
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
