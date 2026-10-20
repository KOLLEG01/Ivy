// Execute the frozen original server against an explicit fake runtime/home.
import { readFileSync, writeFileSync, mkdirSync, lstatSync, realpathSync } from 'node:fs';
import { resolve, join, dirname, relative, isAbsolute } from 'node:path';
import assert from 'node:assert/strict';
import { DatabaseSync } from 'node:sqlite';
const spec = JSON.parse(readFileSync(0, 'utf8'));
const root = resolve(spec.root);
assert.equal(realpathSync(root), root);
assert(!lstatSync(root).isSymbolicLink());
const substitute = value => {
  if (typeof value === 'string') return value.replaceAll('__ROOT__', root);
  if (Array.isArray(value)) return value.map(substitute);
  if (value && typeof value === 'object') return Object.fromEntries(Object.entries(value).map(([k,v])=>[k,substitute(v)]));
  return value;
};
for (const file of spec.files ?? []) {
  const path=resolve(root,file.path),child=relative(root,path);
  assert(child && child!=='..' && !child.startsWith('..\\') && !child.startsWith('../') && !isAbsolute(child));
  mkdirSync(dirname(path),{recursive:true}); writeFileSync(path,Buffer.from(file.base64,'base64'));
}
for (const key of Object.keys(process.env))
  if (/^(?:CLAUDE|ANTHROPIC|CODEX_SHIM|CODEX_HOME)/.test(key)) delete process.env[key];
Object.assign(process.env, { CLAUDE_CODEX_HOME: root, CODEX_HOME: root,
  CLAUDE_CODEX_DISABLE_CODEX_PROXY: '1', CLAUDE_CODEX_DEFAULT_MODEL: 'sonnet',
  CLAUDE_CODEX_DEFAULT_EFFORT: 'medium', CLAUDE_CODEX_COMPAT_VERSION: '0.142.3',
  CLAUDE_CODEX_VERSION_SUFFIX: 'claude-codex', CLAUDE_CODEX_LOG_LEVEL: 'off' });
Object.assign(process.env,spec.environment??{});
let now = spec.now ?? 1700000000123, next = 0;
Date.now = () => now;
globalThis.__ivyClaudeFixture = { home: root, nextId: () => {
  assert(next < spec.ids.length, 'Fixture ids exhausted'); return spec.ids[next++];
} };
const { SessionStore, CodexClaudeAppServer } = await import('./agent-claude-protocol-reference.mjs');
if (spec.config) writeFileSync(join(root, 'config.json'), JSON.stringify(spec.config));
const dbPath = join(root, 'state.sqlite');
const store = new SessionStore(dbPath);
const messages = [], contexts = [], calls = [], active = new Map(), replies = [];
const peers = new Map();
const peer = id => {
  if (!peers.has(id)) peers.set(id, { id, close() {}, send: message => messages.push({ peer: id, message: structuredClone(message) }) });
  return peers.get(id);
};
const runtime = {
  async supportedModels() { calls.push({ method: 'models' }); return spec.models ?? []; },
  async planUsage() { calls.push({ method: 'usage' }); if (spec.usageError) throw new Error(spec.usageError); return spec.usage ?? null; },
  runTurn(context, handlers) {
    contexts.push(structuredClone(context)); calls.push({ method: 'run', turnId: context.turnId });
    return new Promise((resolve, reject) => active.set(context.turnId, { handlers, resolve, reject }));
  },
  async steer(threadId, prompt) { calls.push({ method: 'steer', threadId, prompt }); },
  async interrupt(threadId) { calls.push({ method: 'interrupt', threadId }); },
  async stop() { calls.push({ method: 'stop' }); },
};
const server = new CodexClaudeAppServer(store, runtime);
const records = [];
let stopped = false;
const drain = () => new Promise(setImmediate);
const snapshot = () => {
  const db = new DatabaseSync(dbPath, { readOnly: true });
  try {
    return { threads: db.prepare('select * from threads order by id').all(),
      turns: db.prepare('select * from turns order by id').all(),
      messages: structuredClone(messages), calls: structuredClone(calls),
      contexts: structuredClone(contexts), replies: structuredClone(replies),
      idCalls: next, active: server.hasActiveTurns() };
  } finally { db.close(); }
};
for (const rawStep of spec.steps) {
  const step=substitute(rawStep);
  if (step.now != null) now = step.now;
  let failure = null;
  try {
    if (step.action === 'request' || step.action === 'reply' || step.action === 'notification')
      await server.handle(peer(step.peer ?? 'p1'), step.message);
    else if (step.action === 'batch') await Promise.all(step.messages.map(message=>server.handle(peer(step.peer ?? 'p1'),message)));
    else if (step.action === 'event') await active.get(step.turnId).handlers.onEvent(step.event);
    else if (step.action === 'permission' || step.action === 'input') {
      const handlers = active.get(step.turnId).handlers;
      const value = step.action === 'permission' ? handlers.onPermissionRequest(step.event) : handlers.onUserInputRequest(step.event);
      void Promise.resolve(value).then(result => replies.push({ callback: step.callback, result }), error => replies.push({ callback: step.callback, error: { message: error.message } }));
    } else if (step.action === 'finish') {
      const turn = active.get(step.turnId); active.delete(step.turnId);
      if (step.error) turn.reject(new Error(step.error)); else turn.resolve();
    } else if (step.action === 'disconnect') server.closePeer(peer(step.peer));
    else if (step.action === 'wait') await new Promise(resolve=>setTimeout(resolve,step.millis));
    else if (step.action === 'stop') { await server.stop(); stopped = true; }
    else throw new Error('Unknown fixture action: ' + step.action);
  } catch (error) { failure = { name: error.name, message: error.message }; }
  await drain();
  records.push({ action: step.action, failure, state: snapshot() });
}
if (!stopped) await server.stop();
process.stdout.write(JSON.stringify({ records, environment: Object.fromEntries(
  Object.keys(spec.environment ?? {}).map(key => [key, process.env[key]])
) }));
