import { registerHooks, stripTypeScriptTypes } from 'node:module';
import { existsSync, readFileSync, mkdirSync, writeFileSync } from 'node:fs';
import { readFile } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
import { resolve, join } from 'node:path';
import { randomUUID } from 'node:crypto';
import { tree } from './hive-ui-files-fixture.mjs';

registerHooks({
  resolve(specifier, context, next) {
    if (specifier.startsWith('.') && specifier.endsWith('.js') && context.parentURL) {
      const source = new URL(specifier.slice(0, -3) + '.ts', context.parentURL);
      if (existsSync(fileURLToPath(source))) return next(source.href, context);
    }
    return next(specifier, context);
  },
  load(url, context, next) {
    if (url.endsWith('.ts')) return { format: 'module', source: stripTypeScriptTypes(readFileSync(new URL(url), 'utf8'), { mode: 'transform' }), shortCircuit: true };
    return next(url, context);
  },
});
const { readUiBundle } = await import('../../packages/cli/src/publish-ui.ts');
const { newOperationId } = await import('../../packages/sdk/src/client.ts');
const { deriveOperationId } = await import('../../packages/contracts/src/operation-id.ts');
const { digest } = await import('../../packages/contracts/src/canonical.ts');
const { IvyError, requireThat } = await import('../../packages/contracts/src/errors.ts');
const { UiFiles } = await import('../../services/hive/src/ui-files.ts');
const input = JSON.parse(readFileSync(0, 'utf8'));
const source = stripTypeScriptTypes(readFileSync(new URL('../../services/hive/src/server.ts', import.meta.url), 'utf8'), { mode: 'transform' });
const begin = source.indexOf('rpcClient(principalId) {'), end = source.indexOf('async start()', begin);
if (begin < 0 || end < begin || !source.slice(begin, end).includes('async installUiPackage(input) {')) throw Error('Original package installer boundaries changed');
// Execute both original server methods unchanged, with only their existing owners supplied.
const Original = new Function('digest', 'randomUUID', 'resolve', 'readFile', 'join', 'requireThat', 'readUiBundle', 'newOperationId', 'deriveOperationId',
  'return class {' + source.slice(begin, end) + '};')(digest, randomUUID, resolve, readFile, join, requireThat, readUiBundle, newOperationId, deriveOperationId);
const original = new Original(), trace = [];
original.uiFiles = new UiFiles(input.filesRoot);
const directory = resolve(input.artifactRoot, input.manifest.app?.dist ?? 'dist/ui');
const injected = method => new IvyError('fixture_failure', 'Fixture worker rejected ' + method + '.', 'unknown', { method });
original.worker = {
  async request(message) {
    const record = structuredClone(message);
    if (record.request) {
      if (!/^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/.test(record.request.id)) throw Error('Original request UUID is invalid');
      delete record.request.id;
    }
    trace.push({ request: record, files: tree(input.filesRoot) });
    const method = message.request?.method ?? message.action;
    if (input.fail === method || (input.fail === 'second-contract' && method === 'contracts.register' && trace.filter(row => row.request.request?.method === method).length === 2)) throw injected(method);
    if (method === 'ui.pointers') {
      if (input.change) writeFileSync(join(directory, input.change.path), Buffer.from(input.change.bytes, 'base64'));
      return input.current === null ? [] : [{ current_release_id: input.current }];
    }
    if (method === 'system.status') return { value: input.invalidEpoch ? {} : { runtimeEpoch: 'fixture-package-epoch' } };
    return { value: { fixture: true } };
  },
};
if (input.initialFiles) {
  for (const file of input.initialFiles) {
    const path = join(input.filesRoot, file.path); mkdirSync(resolve(path, '..'), { recursive: true }); writeFileSync(path, Buffer.from(file.bytes, 'base64'));
  }
}
const realNow = Date.now;
if (input.milliseconds !== null) Date.now = () => input.milliseconds;
let result;
try {
  await original.installUiPackage({ manifest: input.manifest, artifactRoot: input.artifactRoot, publisherPrincipalId: input.principal, buildId: input.buildId });
  result = { ok: true };
} catch (error) {
  result = { ok: false, error: IvyError.from(error).toWire() };
} finally { Date.now = realNow; }
process.stdout.write(JSON.stringify({ result, trace, files: tree(input.filesRoot) }));
