import assert from 'node:assert/strict';
import { createReadStream } from 'node:fs';
import { readFile, realpath, stat } from 'node:fs/promises';
import { createHash } from 'node:crypto';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { parseArgs } from 'node:util';

// Read-only comparison. No native process, service, journal, model turn or credential mutation.
const { values } = parseArgs({ options: Object.fromEntries(['config', 'distribution', 'mode'].map(key => [key, { type: 'string' }])) });
for (const key of ['config', 'distribution', 'mode']) assert.ok(values[key], 'Missing --' + key);
assert.ok(['stream', 'buffer'].includes(values.mode));
const config = JSON.parse(await readFile(values.config, 'utf8'));
const agent = config.instances.find(row => row.componentId === 'agent-manager'); assert.ok(agent);
const executable = await realpath(agent.settings.nativeExecutable), size = (await stat(executable)).size;
const { fileHash } = await import(pathToFileURL(join(resolve(values.distribution), 'dist/packages/host-runtime/src/artifact.js')).href);
const memory = () => ({ ...process.memoryUsage(), maximumRssKiB: process.resourceUsage().maxRSS });
const report = { schemaVersion: 1, mode: values.mode, node: process.version, hostId: config.hostId,
  nativeVersion: agent.settings.nativeVersion, nativeExecutableHash: agent.settings.nativeExecutableHash,
  executableBytes: size, startedAt: new Date().toISOString(), before: memory(), passes: [] };
for (let index = 0; index < 2; index++) {
  const began = performance.now(); let actual;
  if (values.mode === 'buffer') actual = await fileHash(executable);
  else {
    const hash = createHash('sha256');
    for await (const bytes of createReadStream(executable)) hash.update(bytes);
    actual = 'sha256:' + hash.digest('hex');
  }
  assert.equal(actual, agent.settings.nativeExecutableHash);
  report.passes.push({ index, elapsedMs: performance.now() - began, memory: memory() });
}
report.completedAt = new Date().toISOString();
console.log(JSON.stringify(report));
