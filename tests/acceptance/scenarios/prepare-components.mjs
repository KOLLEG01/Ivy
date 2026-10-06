import assert from 'node:assert/strict';
import { readFile, realpath, mkdir, copyFile } from 'node:fs/promises';
import { resolve, join, dirname } from 'node:path';
import { pathToFileURL } from 'node:url';
import { parseArgs } from 'node:util';

// One fixed bootstrap/configuration for the whole batch; ordinary prepare owns all checks,
// sharing and candidate publication. No copied/fabricated verification receipts or deployment.
const { values } = parseArgs({ options: {
  ...Object.fromEntries(['distribution', 'config', 'source', 'components', 'evidence'].map(key => [key, { type: 'string' }])),
  execute: { type: 'boolean' }
} });
for (const key of ['distribution', 'config', 'source', 'components', 'evidence']) assert.ok(values[key], 'Missing --' + key);
const distribution = await realpath(resolve(values.distribution));
const load = path => import(pathToFileURL(join(distribution, 'dist', path)).href);
const { inside, atomicJson } = await load('packages/host-runtime/src/config.js');
const { sourceBuildPlan } = await load('packages/host-runtime/src/source.js');
const { hashJson } = await load('packages/contracts/src/canonical.js');
const { validateHost } = await load('packages/contracts/src/validation.js');
const local = await realpath(resolve('.local'));
const configPath = await realpath(resolve(values.config)), source = await realpath(resolve(values.source));
assert.ok(inside(local, configPath) && inside(local, source));
const configBytes = await readFile(configPath), config = JSON.parse(configBytes);
validateHost('HostConfig', config);
assert.ok(config.hostId.endsWith('-acceptance') && !config.restoredFrom);
assert.deepEqual(config.instances, [], 'Use a preparation-only host without service instances.');
assert.equal(new URL(config.publicBaseUrl).hostname, '127.0.0.1');
assert.equal(new URL(config.publicBaseUrl).protocol, 'http:');
for (const root of [config.runtimeRoot, config.artifactRoot, config.stagingRoot]) {
  const actual = await realpath(root); assert.ok(inside(local, actual));
  assert.ok(!inside(actual, source) && !inside(source, actual));
}
const components = values.components.split(',');
assert.ok(components.length > 0 && components.length <= 16);
assert.equal(new Set(components).size, components.length);
for (const component of components) {
  assert.match(component, /^[a-z][a-z0-9-]*$/);
  const plan = await sourceBuildPlan(source, component);
  assert.equal(plan.checks.length, 0, 'Deployment preparation has no general diagnostic checks.');
}
const evidence = resolve(values.evidence);
assert.ok(inside(local, await realpath(dirname(evidence))));
for (const root of [source, config.runtimeRoot, config.artifactRoot, config.stagingRoot]) assert.ok(!inside(root, evidence) && !inside(evidence, root));
await mkdir(evidence);
const report = { schemaVersion: 1, hostId: config.hostId, configHash: hashJson(config), distribution, source,
  components, execute: Boolean(values.execute), startedAt: new Date().toISOString(), phase: 'preflight_passed', candidates: [] };
const entries = [];
const save = async phase => {
  report.phase = phase; await atomicJson(join(evidence, 'report.json'), report);
  console.log(JSON.stringify({ phase, completedComponents: entries.length, reportPath: join(evidence, 'report.json') }));
};
await save('preflight_passed');
try {
  if (values.execute) {
    const { cli } = await load('packages/cli/src/main.js');
    const { fileHash } = await load('packages/host-runtime/src/artifact.js');
    const { runCommand } = await load('packages/host-runtime/src/process.js');
    for (const component of components) {
      assert.deepEqual(await readFile(configPath), configBytes, 'Preparation configuration changed.');
      await save('preparing-' + component);
      const started = performance.now();
      const result = await cli(['prepare', '--config', configPath, '--source', source, '--component', component, '--json']);
      if (result.exitCode !== 0) {
        report.failure = { component, code: result.output.code };
        await save('failed'); process.exitCode = 1; break;
      }
      assert.equal(result.output.code, 'candidate_prepared');
      const candidate = result.output.data; validateHost('Candidate', candidate);
      assert.equal(candidate.componentId, component);
      const path = join(evidence, component + '-candidate.json'); await atomicJson(path, candidate);
      const transfer = join(evidence, component + '-transfer'); await mkdir(transfer);
      const archive = join(transfer, 'artifact.tar.gz');
      await runCommand({ executable: 'tar', args: ['-czf', archive, '-C', candidate.artifactRoot, '.'], timeoutMs: 180000 }, distribution, config.executables);
      await copyFile(candidate.manifestPath, join(transfer, 'component.json'));
      const archiveHash = await fileHash(archive);
      entries.push({ candidate: path, incoming: transfer, candidateId: candidate.candidateId, archiveHash });
      report.candidates.push({ component, candidateId: candidate.candidateId, buildId: candidate.buildId,
        archiveHash, elapsedMs: Math.round(performance.now() - started) });
      await atomicJson(join(evidence, 'candidates.json'), entries);
      await save('prepared-' + component);
    }
    if (!report.failure) {
      assert.deepEqual(await readFile(configPath), configBytes);
      report.completedAt = new Date().toISOString(); await save('passed');
    }
  }
} catch (error) {
  // Avoid serializing assertion diffs that may contain configuration credentials.
  report.failure = { code: error.code ?? error.name, phase: report.phase };
  await save('failed'); process.exitCode = 1;
}
