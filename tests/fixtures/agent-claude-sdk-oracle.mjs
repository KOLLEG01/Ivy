// Frozen native SDK wrapper, with only an explicit home, clock and fake SDK.
import { readFileSync, writeFileSync, mkdirSync, realpathSync, lstatSync, readdirSync } from 'node:fs';
import { resolve, dirname, join, relative, isAbsolute } from 'node:path';
import assert from 'node:assert/strict';
const spec = JSON.parse(readFileSync(0, 'utf8'));
const root = resolve(spec.root);
assert.equal(realpathSync(root), root); assert(!lstatSync(root).isSymbolicLink());
const replace = value => {
  if (typeof value === 'string') return value.replaceAll('__ROOT__', root);
  if (Array.isArray(value)) return value.map(replace);
  if (value && typeof value === 'object') return Object.fromEntries(Object.entries(value).map(([k,v])=>[k,replace(v)]));
  return value;
};
for (const file of spec.files ?? []) {
  const path = resolve(root, file.path), child = relative(root, path);
  assert(child && child !== '..' && !child.startsWith('..\\') && !child.startsWith('../') && !isAbsolute(child));
  mkdirSync(dirname(path), { recursive: true }); writeFileSync(path, file.text);
}
let next = 0;
globalThis.__ivyClaudeFixture = { home: root, nextId: () => 'fixture-' + next++ };
const { NativeClaudeRuntime } = await import('./agent-claude-protocol-reference.mjs');
const rows = [];
for (const raw of spec.rows) {
  for (const key of Object.keys(process.env)) if (/^(?:CLAUDE|ANTHROPIC|CODEX_SHIM|CODEX_HOME)/.test(key)) delete process.env[key];
  const row = replace(raw);
  Object.assign(process.env, row.environment);
  const runtime = new NativeClaudeRuntime();
  let result;
  try {
    const context = row.context;
    const options = runtime.buildOptions({}, context, new AbortController());
    const permissionCallback = typeof options.canUseTool === 'function';
    const calls = [];
    runtime.turns.set(context.turnId, { handlers: {
      onPermissionRequest: async event => { calls.push(event); return row.decision ?? { decision: 'accept' }; },
      onUserInputRequest: async event => { calls.push(event); return row.answers ?? { answers: {} }; },
    } });
    const responses = [];
    for (const callback of row.callbacks ?? []) {
      responses.push(permissionCallback ? await options.canUseTool(callback.name, callback.input, { toolUseID: callback.id, signal: new AbortController().signal }) : null);
    }
    delete options.canUseTool; delete options.abortController;
    result = { options, messages: await Array.fromAsync(runtime.buildPromptIterable(context)), permissionCallback, calls, responses };
  } catch (error) { result = { error: { name: error.name, message: error.message } }; }
  rows.push(result);
}
const files = [];
function tree(dir) { for (const name of readdirSync(dir).sort()) {
  const path = join(dir, name), stat = lstatSync(path);
  assert(!stat.isSymbolicLink());
  if (stat.isDirectory()) tree(path);
  else files.push({ path: relative(root,path).replaceAll('\\','/'), base64: readFileSync(path).toString('base64') });
} }
tree(root);
process.stdout.write(JSON.stringify({ rows, files }));
