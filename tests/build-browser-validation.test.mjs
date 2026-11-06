import test from 'node:test';
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { mkdir, mkdtemp, rm, writeFile } from 'node:fs/promises';
import { dirname, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { build } from 'vite';
import { browserAgentValidation } from '../tools/build/browser-agent-validation.mjs';

test('browser client starts and keeps request guards without runtime code generation', async () => {
  const project = fileURLToPath(new URL('../', import.meta.url));
  const cache = resolve(project, 'dist/.build-cache');
  await mkdir(cache, { recursive: true });
  const directory = await mkdtemp(resolve(cache, 'browser-client-test-'));
  try {
    const source = resolve(project, 'packages/sdk/src/client.ts').replaceAll('\\', '/');
    const entry = resolve(directory, 'entry.mjs');
    await writeFile(entry, `export { browserClient } from ${JSON.stringify(source)};`);
    const result = await build({ configFile: false, root: project, logLevel: 'silent',
      build: { write: false, minify: false, target: 'es2023',
        lib: { entry, formats: ['es'], fileName: 'client' } } });
    const output = (Array.isArray(result) ? result : [result]).flatMap(item => item.output).filter(item => item.type === 'chunk');
    assert.equal(output.length, 1);
    const bundle = resolve(directory, 'client.mjs');
    await writeFile(bundle, output[0].code);
    const checks = `import assert from 'node:assert/strict';
import { browserClient } from ${JSON.stringify(pathToFileURL(bundle).href)};
assert.throws(() => Function('return 1'), EvalError);
globalThis.location = { origin: 'https://browser-fixture.invalid' };
globalThis.fetch = async (url, options) => {
  assert.equal(String(url), 'https://browser-fixture.invalid/api/v1/rpc');
  assert.equal(options.credentials, 'same-origin');
  assert.equal(options.headers.Authorization, undefined);
  const request = JSON.parse(options.body);
  const reply = value => ({ jsonrpc: '2.0', id: value.id, result: { items: [], nextCursor: null } });
  return new Response(JSON.stringify(Array.isArray(request) ? request.map(reply) : reply(request)));
};
const client = browserClient('https://browser-fixture.invalid/');
assert.equal(browserClient('https://browser-fixture.invalid/'), client);
assert.throws(() => browserClient('https://another-origin.invalid/'), error => error.code === 'invalid_arguments');
assert.deepEqual(await client.request('uis.list', { limit: 10 }), { items: [], nextCursor: null });
`;
    execFileSync(process.execPath, ['--disallow-code-generation-from-strings', '--input-type=module', '-e', checks], { stdio: 'pipe' });
  } finally {
    assert.equal(dirname(directory), cache);
    await rm(directory, { recursive: true, force: true });
  }
});

test('browser Agent contracts validate without runtime code generation', async () => {
  const project = fileURLToPath(new URL('../', import.meta.url));
  const cache = resolve(project, 'dist/.build-cache');
  await mkdir(cache, { recursive: true });
  const directory = await mkdtemp(resolve(cache, 'browser-validation-test-'));
  try {
    const source = resolve(project, 'packages/contracts/src/agent-validation.ts').replaceAll('\\', '/');
    const entry = resolve(directory, 'entry.mjs');
    await writeFile(entry, `export { validateAgent, validateAgentFrame } from ${JSON.stringify(source)};`);
    const result = await build({ configFile: false, root: project, logLevel: 'silent',
      plugins: [browserAgentValidation(project, directory)],
      build: { write: false, minify: false, target: 'es2023',
        commonjsOptions: { include: [/node_modules/, /browser-agent-validation/] },
        lib: { entry, formats: ['es'], fileName: 'validation' } } });
    const output = (Array.isArray(result) ? result : [result]).flatMap(item => item.output).filter(item => item.type === 'chunk');
    assert.equal(output.length, 1);
    const bundle = resolve(directory, 'validation.mjs');
    await writeFile(bundle, output[0].code);
    const checks = `import assert from 'node:assert/strict';
import { validateAgent, validateAgentFrame } from ${JSON.stringify(pathToFileURL(bundle).href)};
assert.throws(() => Function('return 1'), EvalError);
validateAgent('BackendsResult', { backends: [] });
for (const value of [null, {}, {backends: [{}]}, {backends: 'invalid'}])
  assert.throws(() => validateAgent('BackendsResult', value), error => error.code === 'invalid_arguments');
for (const name of ['constructor', '__proto__', 'missing'])
  assert.throws(() => validateAgent(name, {}), error => error.code === 'invalid_arguments');
validateAgentFrame({ result: null });
validateAgentFrame('original-id', 'request-id');
assert.throws(() => validateAgentFrame({result: null, error: {code: 1, message: 'ambiguous'}}), error => error.code === 'native_invalid_frame');
assert.throws(() => validateAgentFrame(Number.MAX_SAFE_INTEGER + 1, 'request-id'), error => error.code === 'native_invalid_frame');
`;
    execFileSync(process.execPath, ['--disallow-code-generation-from-strings', '--input-type=module', '-e', checks], { stdio: 'pipe' });
  } finally {
    assert.equal(dirname(directory), cache);
    await rm(directory, { recursive: true, force: true });
  }
});
