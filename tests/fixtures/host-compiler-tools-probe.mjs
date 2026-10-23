import assert from 'node:assert/strict';
import {readFile, writeFile, realpath} from 'node:fs/promises';
import {stripTypeScriptTypes} from 'node:module';
import {join, resolve} from 'node:path';
import {startProcess, runtimeEnvironment} from '../../dist/packages/host-runtime/src/process.js';

// Execute the current producer without requiring a complete service build.
const moduleUrl = source => 'data:text/javascript;base64,' + Buffer.from(source).toString('base64');
const errors = moduleUrl(stripTypeScriptTypes(await readFile('packages/contracts/src/errors.ts', 'utf8'), {mode: 'transform'}));
const source = stripTypeScriptTypes(await readFile('packages/host-runtime/src/preparation-tools.ts', 'utf8'))
  .replace("'../../contracts/src/errors.js'", JSON.stringify(errors));
const {preparationTools} = await import(moduleUrl(source));
const root = resolve(process.argv[2]);
const executables = {cargo: process.env.IVY_TEST_CARGO, rustc: process.env.IVY_TEST_RUSTC, cl: process.env.IVY_TEST_CL,
  node: join(root, 'selected node runtime/node.exe')};
const tools = await preparationTools(executables, root, root);
assert.equal(tools.environment.IVY_CARGO, await realpath(executables.cargo));
assert.equal(tools.environment.RUSTC, await realpath(executables.rustc));
assert.ok(tools.environment.CARGO_HOME);
assert.ok(tools.environment.INCLUDE);
assert.ok(tools.environment.LIB);
assert.equal(runtimeEnvironment(tools.environment).IVY_COMPILER_SECRET, undefined);
// Exercise the npm lifecycle boundary, including an explicit node mapping that
// differs from this producer's own process.execPath and contains a space.
for (const mappings of [executables, {}]) {
  const selected = await preparationTools(mappings, root, root);
  const child = await startProcess({executable: 'shell', args: ['/d', '/s', '/c', 'node ./engine-requirements.js'], timeoutMs: 30000},
    root, {shell: process.env.ComSpec}, {environment: selected.environment});
  const result = await child.completion;
  assert.equal(result.exitCode, 0, JSON.stringify(result));
  assert.equal(result.errorCode, null);
  assert.equal(await realpath(result.stdout.trim()), await realpath(mappings.node ?? process.execPath));
}
await writeFile(join(root, 'headers-ts.c'), '#include <stddef.h>\nint header_probe(void) { return sizeof(size_t); }\n');
for (const [executable, args] of [
  ['cargo', ['--version']], ['rustc', ['--print', 'sysroot']],
  ['cl', ['/nologo', '/c', join(root, 'headers-ts.c'), '/Fo' + join(root, 'headers-ts.obj')]],
]) {
  const child = await startProcess({executable, args, timeoutMs: 30000}, root, executables,
    {environment: tools.environment, useJobLauncher: false});
  const result = await child.completion;
  assert.equal(result.exitCode, 0, JSON.stringify(result));
  assert.equal(result.errorCode, null);
}
console.log(JSON.stringify({include: Boolean(tools.environment.INCLUDE), lib: Boolean(tools.environment.LIB)}));
