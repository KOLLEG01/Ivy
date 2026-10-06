import assert from 'node:assert/strict';
import { test } from 'node:test';
import type { TestContext } from 'node:test';
import { mkdtemp, mkdir, readFile, writeFile, rm, symlink } from 'node:fs/promises';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
import { InstructionsManager, instructionsName, composeInstructions } from '../services/agent-manager/src/instructions.js';
import { ExecutorLock } from '../packages/host-runtime/src/journal.js';
import { digest } from '../packages/contracts/src/canonical.js';
import { IvyError } from '../packages/contracts/src/errors.js';
import type { Agent } from '../packages/contracts/src/generated.js';
import type { ServiceConnection } from '../packages/sdk/src/service.js';
import { hiveMcpInstructions, hiveMcpAgentSummary, composeHiveMcpInstructions } from '../instructions/hive-mcp.js';

const defaults: Agent.InstructionsDocument = {
  schemaVersion: 1,
  hostId: null,
  enabled: true,
  includeLocal: true,
  text: 'Project default',
};

test('always-loaded Ivy summary shares the wiki and usage paragraphs with full instructions', () => {
  assert.deepEqual(hiveMcpAgentSummary.split('\n\n').slice(1), hiveMcpInstructions.split('\n\n').slice(1, 4));
  assert.doesNotMatch(hiveMcpAgentSummary, /Tool workflows:|Use a service's specialized tools/);
});

async function fixture(t: TestContext) {
  const root = await mkdtemp(join(tmpdir(), 'ivy-instructions-')), home = join(root, 'codex'); await mkdir(home);
  t.after(async () => { await rm(root, { recursive: true, force: true }); });
  const documents = new Map<string, { document: Agent.InstructionsDocument; revision: number }>();
  let failure = false;
  let discoveredInstructions: string | null = hiveMcpInstructions;
  let legacyServices: Array<{ serviceName: string; description: string }> = [];
  // Only the document source is simulated; files, atomic replacement and OS ownership locks are real.
  const connection = { request: (async (method: string, params: Record<string, unknown>) => {
    if (failure) throw new IvyError('connection_lost', 'Synthetic Hive outage.');
    if (method === 'discovery.instructions') {
      if (discoveredInstructions === null) throw new IvyError('not_found', 'Older Hive has no composed instructions.');
      return { instructions: discoveredInstructions };
    }
    if (method === 'discovery.list') return { items: legacyServices.map(value => ({ kind: 'service', name: value.serviceName,
      serviceName: value.serviceName, description: value.description })), nextCursor: null };
    const key = String(params['path'] ?? params['objectId']).replace(/^\//, ''), value = documents.get(key);
    if (!value) throw new IvyError('not_found', 'Absent document.');
    const object = { id: key, currentRevision: value.revision, effectivelyArchived: false, contractKey: 'agent/instructions' };
    if (method === 'objects.stat') return object;
    assert.equal(method, 'objects.read');
    return { object, revision: { revision: value.revision, contractVersion: '1.0.0', contentHash: digest(JSON.stringify(value.document)) },
      content: { encoding: 'json', value: value.document } };
  }) as ServiceConnection['request'] };
  const put = (hostId: string | null, text: string, enabled: boolean | null = true, includeLocal = true) => {
    const name = instructionsName(hostId), prior = documents.get(name);
    documents.set(name, { document: { schemaVersion: 1, hostId, text, enabled, includeLocal }, revision: (prior?.revision ?? 0) + 1 });
  };
  const manager = new InstructionsManager(home, 'host-a', 'http://127.0.0.1:1234/ivy', defaults);
  return { root, home, documents, put, manager, connection, offline: (value: boolean) => { failure = value; },
    setDiscoveredInstructions: (value: string) => { discoveredInstructions = value; },
    setLegacyServices: (values: Array<{ serviceName: string; description: string }>) => {
      discoveredInstructions = null; legacyServices = values;
    }, target: join(home, 'AGENTS.override.md') };
}

test('project defaults, Hive and host instructions compose in precedence order and explicit disabling restores user bytes', async t => {
  const f = await fixture(t);
  const original = Buffer.from('Original override — Grüße\r\n');
  await writeFile(f.target, original); await writeFile(join(f.home, 'AGENTS.md'), 'Local baseline');
  f.put(null, 'Hive preference'); f.put('host-a', 'Host preference');
  await f.manager.synchronize(f.connection); assert.equal(f.manager.status.state, 'applied');
  const output = await readFile(f.target, 'utf8');
  assert.ok(output.endsWith('\n\n@AGENTS.md\n'));
  assert.doesNotMatch(output, /Managed by Ivy AgentManager|## Hive instructions|## Hive MCP instructions/);
  assert.ok(output.indexOf('Local baseline') < output.indexOf('Hive preference'));
  assert.ok(output.indexOf('Hive preference') < output.indexOf('Host preference'));
  assert.ok(output.indexOf('Host preference') < output.indexOf(hiveMcpAgentSummary));
  assert.doesNotMatch(output, /Use a service's specialized tools|Tool workflows:/);
  const version = f.manager.status.appliedVersion;
  await f.manager.synchronize(f.connection); assert.equal(f.manager.status.appliedVersion, version); assert.equal(await readFile(f.target, 'utf8'), output);
  f.put('host-a', '', false); await f.manager.synchronize(f.connection);
  assert.equal(f.manager.status.state, 'disabled'); assert.deepEqual(await readFile(f.target), original);
  f.put('host-a', '', null); await f.manager.synchronize(f.connection);
  assert.equal(f.manager.status.state, 'applied'); assert.ok((await readFile(f.target, 'utf8')).includes('Hive preference'));
  assert.ok(!(await readFile(f.target, 'utf8')).includes('Host preference'));
  f.put(null, '', null); await f.manager.synchronize(f.connection);
  assert.equal(f.manager.status.state, 'applied'); assert.ok((await readFile(f.target, 'utf8')).includes('Project default'));
  f.put(null, '', false); await f.manager.synchronize(f.connection);
  assert.equal(f.manager.status.state, 'disabled'); assert.deepEqual(await readFile(f.target), original);
  assert.equal(await readFile(join(f.home, 'AGENTS.md'), 'utf8'), 'Local baseline');
});

test('managed output uses the requested headings and keeps the local reference last', () => {
  const global = { document: { schemaVersion: 1 as const, hostId: null, enabled: true, includeLocal: true,
    text: '# Actions attributed to the user\n\nKeep this guidance.' }, objectId: 'global', revision: 1, contentHash: digest('global') };
  const output = composeInstructions(global, null, Buffer.from('Local guidance'), null, 'MCP guidance.');
  assert.equal(output.body?.toString('utf8'), '## Local user instructions\n\nLocal guidance\n\n## Actions attributed to the user\n\nKeep this guidance.\n\n## Ivy MCP\n\nMCP guidance.\n\n@AGENTS.md\n');
});

test('managed instructions keep service hints but load workflows only on demand', async t => {
  const f = await fixture(t);
  f.put(null, 'Hive preference');
  await f.manager.synchronize(f.connection);
  const initial = await readFile(f.target, 'utf8');
  const discovered = composeHiveMcpInstructions([{ serviceName: 'example', discoveryHint: 'Example capability.' }],
    ['example/example: Detailed workflow with several tool calls.']);
  f.setDiscoveredInstructions(discovered);
  await f.manager.synchronize(f.connection);
  const changed = await readFile(f.target, 'utf8');
  assert.notEqual(changed, initial);
  assert.match(discovered, /Tool workflows:/);
  assert.ok(changed.endsWith(`${hiveMcpAgentSummary}\n\nServices registered to this Hive provide the following capabilities:\n- example: Example capability.\n\n@AGENTS.md\n`));
  assert.doesNotMatch(changed, /Tool workflows:|Detailed workflow with several tool calls/);
  assert.doesNotMatch(changed, /Use a service's specialized tools/);
  assert.match(changed, /Call ivy_instructions for complete guidance/);
  assert.ok(changed.indexOf('Hive preference') < changed.indexOf('Example capability.'));
});

test('older Hive service discovery supplies the MCP capability list', async t => {
  const f = await fixture(t);
  f.put(null, 'Hive preference');
  f.setLegacyServices([{ serviceName: 'agent-manager', description: 'Manage native Codex tasks.' }]);
  await f.manager.synchronize(f.connection);
  assert.equal(f.manager.status.state, 'applied');
  const output = await readFile(f.target, 'utf8');
  assert.match(output, /Services registered to this Hive provide the following capabilities:/);
  assert.match(output, /- agent-manager: Manage native Codex tasks\./);
});

test('project instructions bootstrap before Hive is reachable and stored Hive instructions replace them', async t => {
  const f = await fixture(t);
  await f.manager.bootstrap();
  assert.equal(f.manager.status.state, 'applied');
  assert.ok((await readFile(f.target, 'utf8')).includes('Project default'));
  f.put(null, 'Stored Hive guidance');
  await f.manager.synchronize(f.connection);
  const output = await readFile(f.target, 'utf8');
  assert.ok(output.includes('Stored Hive guidance'));
  assert.ok(!output.includes('Project default'));
});

test('Hive outage retains the last file and reports pending; user edits conflict even when disabling', async t => {
  const f = await fixture(t); f.put(null, 'Initial'); await f.manager.synchronize(f.connection);
  const original = await readFile(f.target), version = f.manager.status.appliedVersion;
  f.offline(true); await f.manager.synchronize(f.connection);
  assert.equal(f.manager.status.state, 'pending'); assert.deepEqual(await readFile(f.target), original); assert.equal(f.manager.status.appliedVersion, version);
  f.offline(false); await writeFile(f.target, 'User edit'); f.put(null, 'Changed');
  await f.manager.synchronize(f.connection); assert.equal(f.manager.status.state, 'conflict'); assert.equal(await readFile(f.target, 'utf8'), 'User edit');
  f.put(null, '', false); await f.manager.synchronize(f.connection);
  assert.equal(f.manager.status.state, 'conflict'); assert.equal(await readFile(f.target, 'utf8'), 'User edit');
});

test('a failed revision read is not interpreted as removal and excluded local files are not opened', async t => {
  const f = await fixture(t); f.put(null, 'Initial', true, false);
  await mkdir(join(f.home, 'AGENTS.md'));
  await f.manager.synchronize(f.connection); assert.equal(f.manager.status.state, 'applied');
  const before = await readFile(f.target);
  const partial = { request: (async (method: string, params: never) => {
    if (method === 'objects.read') throw new IvyError('not_found', 'Synthetic incomplete source read.');
    return f.connection.request(method as never, params);
  }) as ServiceConnection['request'] };
  await f.manager.synchronize(partial); assert.equal(f.manager.status.state, 'pending'); assert.deepEqual(await readFile(f.target), before);
});

test('shared-home managers serialize and a different host cannot replace the owning policy', async t => {
  const f = await fixture(t); f.put(null, 'Initial'); await f.manager.synchronize(f.connection);
  const peer = new InstructionsManager(f.home, 'host-a', f.manager.hiveUrl, defaults);
  const lock = new ExecutorLock(join(f.home, '.ivy-agent-instructions'));
  try { await peer.synchronize(f.connection); assert.equal(peer.status.state, 'pending'); assert.equal(peer.status.code, 'executor_already_running'); }
  finally { lock.close(); }
  await peer.synchronize(f.connection); assert.equal(peer.status.state, 'applied');
  const foreign = new InstructionsManager(f.home, 'host-b', f.manager.hiveUrl, defaults);
  f.put('host-b', 'Other host'); await foreign.synchronize(f.connection);
  assert.equal(foreign.status.state, 'conflict'); assert.ok(!(await readFile(f.target, 'utf8')).includes('Other host'));
});

test('explicitly disabling instructions without an original file removes only the managed output', async t => {
  const f = await fixture(t); f.put(null, 'Initial'); await f.manager.synchronize(f.connection);
  f.put(null, '', false); await f.manager.synchronize(f.connection);
  assert.equal(f.manager.status.state, 'disabled'); await assert.rejects(readFile(f.target), { code: 'ENOENT' });
});

test('wrong scope and linked user files fail without replacing their targets', async t => {
  const f = await fixture(t); f.put(null, 'Initial');
  f.documents.get(instructionsName(null))!.document.hostId = 'other';
  await f.manager.synchronize(f.connection); assert.equal(f.manager.status.state, 'conflict'); await assert.rejects(readFile(f.target), { code: 'ENOENT' });
  f.put(null, 'Correct'); const outside = join(f.root, 'outside'); await mkdir(outside);
  await symlink(outside, f.target, process.platform === 'win32' ? 'junction' : 'dir');
  await f.manager.synchronize(f.connection); assert.equal(f.manager.status.state, 'conflict');
});

test('composition rejects oversized local guidance rather than truncating it', () => {
  const global = { document: { schemaVersion: 1 as const, hostId: null, text: 'Guidance', enabled: true, includeLocal: true },
    objectId: 'global', revision: 1, contentHash: digest('Guidance') };
  assert.throws(() => composeInstructions(global, null, Buffer.alloc(32769, 'x')), (error: unknown) => error instanceof IvyError && error.code === 'limit_exceeded');
  assert.throws(() => composeInstructions(global, null, Buffer.from([0xff])), (error: unknown) => error instanceof IvyError && error.code === 'target_conflict');
});

test('interrupted replacement with already written target is reconciled without losing the original backup', async t => {
  const f = await fixture(t); await writeFile(f.target, 'Before Ivy'); f.put(null, 'Initial'); await f.manager.synchronize(f.connection);
  const path = join(f.home, '.ivy-agent-instructions/state.json'), state = JSON.parse(await readFile(path, 'utf8'));
  state.pending = { before: state.originalHash, after: state.appliedHash, version: state.appliedVersion, active: true };
  state.appliedHash = state.originalHash; state.appliedVersion = null;
  await writeFile(path, JSON.stringify(state));
  const restarted = new InstructionsManager(f.home, f.manager.hostId, f.manager.hiveUrl, defaults);
  await restarted.synchronize(f.connection); assert.equal(restarted.status.state, 'applied');
  f.put(null, '', false); await restarted.synchronize(f.connection); assert.equal(await readFile(f.target, 'utf8'), 'Before Ivy');
});
