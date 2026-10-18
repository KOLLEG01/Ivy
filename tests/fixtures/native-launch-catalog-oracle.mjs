// Exercise the original launch path, stopping at its deliberately mismatched
// fixture executable before home creation, shell probing, or process launch.
import assert from 'node:assert/strict';
import { mkdir, readFile, writeFile, stat } from 'node:fs/promises';
import { join } from 'node:path';
import { startCodexProcess } from '../../dist/packages/host-runtime/src/codex-process.js';
import { nativeCatalogPath } from '../../dist/packages/sdk/src/node.js';
import { IvyError } from '../../dist/packages/contracts/src/errors.js';
let text = '';
for await (const bytes of process.stdin) text += bytes;
const { root } = JSON.parse(text), rows = [];
const sources = ['codex-0.142.3/catalog.json', ...['0.154.0', '0.158.0', '0.159.2'].flatMap(v => [`codex-${v}/catalog.json`, `codex-${v}/catalog.linux-x64.json`])];
const catalogs = await Promise.all(sources.map(async file => JSON.parse(await readFile(join('specs/native', file), 'utf8'))));
const small = c => ({ ...c, clientRequests: c.clientRequests.filter(r => r.method === 'thread/realtime/start'), serverRequests: [], serverNotifications: [], clientNotifications: [], derivedLegacyTypes: [] });
async function collect(label, source, edit = () => {}) {
  const catalog = small(structuredClone(source));
  const dir = join(root, String(rows.length)), artifact = join(dir, 'artifact');
  const settings = { nativeVersion: source.version, nativeExecutableHash: source.nativeExecutableHash, nativeExecutable: join(dir, 'fixture.exe'), codexHome: join(dir, 'home'), appServer: { mode: 'owned-stdio' } };
  const input = { catalog, version: settings.nativeVersion, hash: settings.nativeExecutableHash };
  edit(input);
  settings.nativeVersion = input.version; settings.nativeExecutableHash = input.hash;
  const file = nativeCatalogPath(join(artifact, 'dist'), settings.nativeVersion);
  await mkdir(join(file, '..'), { recursive: true });
  await writeFile(file, JSON.stringify(input.catalog));
  await writeFile(settings.nativeExecutable, 'not the catalog executable');
  let expected;
  try {
    const connection = await startCodexProcess({ settings, artifactRoot: artifact, dataRoot: join(dir, 'data'), clientName: 'Fixture', clientVersion: '1.0.0', onRequest() { throw Error('Unexpected request'); }, onNotification() { throw Error('Unexpected notification'); }, onClose() {} });
    await connection.close(); throw Error('Fixture must never reach native launch');
  } catch (error) {
    expected = error.code === 'native_build_mismatch' && error.message === 'Configured Codex executable differs from its exact catalog build.'
      ? { ok: true } : { ok: false, error: IvyError.from(error).toWire() };
  }
  assert.equal(await stat(settings.codexHome).then(() => true, () => false), false, 'No home creation before the executable check');
  rows.push({ label, input, expected });
}
for (let i = 0; i < catalogs.length; i++) await collect('retained-' + i, catalogs[i]);
const base = catalogs[1];
await collect('wrong-version', base, x => x.catalog.version = '0.154.1');
await collect('wrong-hash', base, x => x.hash = 'sha256:' + 'f'.repeat(64));
await collect('wrong-provider', base, x => x.catalog.provider = 'other');
await collect('missing-realtime', base, x => x.catalog.clientRequests = []);
await collect('wrong-method', base, x => x.catalog.clientRequests[0].method = 'thread/realtime/other');
await collect('missing-initial-items', base, x => x.catalog.clientRequests[0].inputSchema = {});
await collect('description-substring', base, x => x.catalog.clientRequests[0].inputSchema = { description: 'initialItems' });
await collect('case-sensitive', base, x => x.catalog.clientRequests[0].inputSchema = { description: 'InitialItems' });
await collect('boolean-schema', base, x => x.catalog.clientRequests[0].inputSchema = true);
await collect('malformed-catalog', base, x => x.catalog = {});
assert.equal(rows.find(r => r.label === 'description-substring').expected.ok, true);
assert.equal(rows.find(r => r.label === 'missing-initial-items').expected.error.data.code, 'native_build_mismatch');
process.stdout.write(JSON.stringify(rows));
