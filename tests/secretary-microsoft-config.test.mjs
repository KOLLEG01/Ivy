import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, writeFile, chmod } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { randomUUID } from 'node:crypto';
import { admitSchema, hashJson } from '../dist/packages/sdk/src/node.js';
import { microsoftRegistry, validateMicrosoft } from '../dist/services/secretary/src/microsoft-schema.js';
import { loadMicrosoftConfiguration, microsoftConfiguration } from '../dist/services/secretary/src/microsoft-config.js';
import { settingsFor, principal } from './secretary-fixture.mjs';
import { profile } from './secretary-outlook-fixture.mjs';

const settings = () => settingsFor(randomUUID(), 'secretary', principal, { sources: [{ sourceId: 'mail', accountId: profile.id, kind: 'email', producerPrincipalIds: [principal], ownSenderIds: [profile.email], allowedSenderIds: null }] });
const configuration = () => ({ schemaVersion: 1, collectors: [{ sourceId: 'mail', profile, since: '2026-09-01T00:00:00.000Z', pageSize: 8, pollMs: 60000, maximumPages: 100, threadCwd: resolve('.local/microsoft-source'), expectedAccountHash: hashJson('account'),
  target: { serviceNodeId: 'native', hostId: 'fixture', nativeVersion: '0.149.1', nativeExecutableHash: hashJson('executable'), catalogHash: hashJson('catalogue') } }] });

test('Microsoft contracts admit bounded native evidence and require exact valid configured producer source and selection', () => {
  for (const contract of microsoftRegistry().contracts) if (contract.jsonSchema) admitSchema(contract.jsonSchema);
  const original = configuration(), configured = settings(); assert.deepEqual(microsoftConfiguration(original, configured), original);
  for (const change of [{ since: '2026-02-30T00:00:00.000Z' }, { since: 'invalid' }, { threadCwd: 'relative' }, { pageSize: 33 }, { pollMs: 59999 }, { maximumPages: 0 }, { profile: { ...profile, id: 'another' } }]) {
    assert.throws(() => microsoftConfiguration({ ...original, collectors: [{ ...original.collectors[0], ...change }] }, configured));
  }
  assert.throws(() => microsoftConfiguration(original, { ...configured, sources: [{ ...configured.sources[0], producerPrincipalIds: ['different'] }] }));
  assert.throws(() => microsoftConfiguration({ ...original, collectors: [...original.collectors, ...original.collectors] }, configured));
  const blob = { byteLength: 33554432, contentHash: hashJson('original'), chunks: Array.from({ length: 32 }, () => ({ objectId: randomUUID(), revision: 1 })) };
  validateMicrosoft('MicrosoftBlob', blob); assert.throws(() => validateMicrosoft('MicrosoftBlob', { ...blob, byteLength: 33554433 })); assert.throws(() => validateMicrosoft('MicrosoftBlob', { ...blob, chunks: [...blob.chunks, blob.chunks[0]] }));
});

test('Microsoft source settings load only a bounded protected regular file and an absent file configures no collectors', async () => {
  const root = await mkdtemp(join(tmpdir(), 'ivy-microsoft-config-')), path = join(root, 'config.json'), configured = settings(), original = configuration();
  assert.deepEqual(await loadMicrosoftConfiguration(path, configured), { schemaVersion: 1, collectors: [] });
  await writeFile(path, JSON.stringify({schemaVersion:1,microsoft:original}), { mode: 0o600 }); assert.deepEqual(await loadMicrosoftConfiguration(path, configured), original);
  await writeFile(path, '{'); await assert.rejects(loadMicrosoftConfiguration(path, configured));
  await writeFile(path, ' '.repeat(1024*1024+1)); await assert.rejects(loadMicrosoftConfiguration(path, configured), { code: 'microsoft_config_unprotected' });
  if (process.platform !== 'win32') { await writeFile(path, JSON.stringify({schemaVersion:1,microsoft:original})); await chmod(path, 0o644); await assert.rejects(loadMicrosoftConfiguration(path, configured), { code: 'microsoft_config_unprotected' }); }
});
