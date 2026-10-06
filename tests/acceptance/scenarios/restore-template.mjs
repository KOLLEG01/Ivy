import assert from 'node:assert/strict';
import { readFile, realpath, lstat } from 'node:fs/promises';
import { resolve, join, dirname, basename, relative } from 'node:path';
import { pathToFileURL } from 'node:url';
import { parseArgs } from 'node:util';

// Prepare only a fresh, disabled restore target. Backup, restore and release remain
// separate explicit operations against the installed acceptance configuration.
const { values } = parseArgs({ options: Object.fromEntries(
  ['distribution', 'config', 'source-root', 'target-root', 'port'].map(key => [key, { type: 'string' }])) });
for (const key of ['distribution', 'config', 'source-root', 'target-root', 'port']) assert.ok(values[key], 'Missing --' + key);
assert.match(values.port, /^\d+$/);
const port = Number(values.port); assert.ok(Number.isInteger(port) && port >= 1024 && port <= 65535);
const distribution = await realpath(resolve(values.distribution));
const load = path => import(pathToFileURL(join(distribution, 'dist', path)).href);
const { inside, atomicJson } = await load('packages/host-runtime/src/config.js');
const { privateDirectory, exists } = await load('packages/host-runtime/src/backup-files.js');
const { validateHost } = await load('packages/contracts/src/validation.js');
const { hashJson } = await load('packages/contracts/src/canonical.js');
const { instanceOwnedCodexHome } = await load('packages/host-runtime/src/codex-home.js');
const { servicePaths, hostConfigurationPath, resolveHostConfiguration } = await load('packages/host-runtime/src/layout.js');
const local = await realpath(resolve('.local')), sourceRoot = await realpath(resolve(values['source-root']));
const configPath = await realpath(resolve(values.config));
assert.ok(inside(local, sourceRoot) && inside(sourceRoot, configPath));
const sourceBytes = await readFile(configPath), source = resolveHostConfiguration(JSON.parse(sourceBytes), configPath);
validateHost('HostConfig', source); assert.ok(source.hostId.endsWith('-acceptance') && !source.restoredFrom);
assert.ok(source.instances.every(instance => instance.engine === 'process'));
for (const root of [source.runtimeRoot, source.artifactRoot, source.stagingRoot]) assert.ok(inside(sourceRoot, await realpath(root)));
const requestedTarget = resolve(values['target-root']);
assert.equal(await exists(requestedTarget), false, 'Never overwrite an earlier target or evidence.');
const parent = await realpath(dirname(requestedTarget)), targetRoot = join(parent, basename(requestedTarget));
assert.ok(inside(local, parent));
assert.ok(!inside(sourceRoot, targetRoot) && !inside(targetRoot, sourceRoot));
assert.equal((await lstat(parent)).isDirectory(), true);
const target = structuredClone(source);
target.runtimeRoot = join(targetRoot, 'runtime'); target.artifactRoot = join(targetRoot, 'artifacts'); target.stagingRoot = join(targetRoot, 'staging');
if (source.servicesRoot) { target.ivyRoot = targetRoot; target.servicesRoot = join(targetRoot, 'services'); target.configPath = join(targetRoot, 'config.json'); }
const endpoint = new URL(source.publicBaseUrl);
assert.equal(endpoint.protocol, 'http:'); assert.equal(endpoint.hostname, '127.0.0.1');
assert.notEqual(Number(endpoint.port || 80), port); endpoint.port = String(port); target.publicBaseUrl = endpoint.href;
assert.equal(target.instances.filter(instance => instance.componentId === 'hive').length, 1);
const internalRoots = new Map();
for (const instance of target.instances) {
  instance.enabled = false;
  const prior = source.instances.find(value => value.instanceId === instance.instanceId);
  if (instance.paths) instance.paths = Object.fromEntries(Object.keys(instance.paths).map(kind => [kind, join(targetRoot, 'explicit-services', instance.instanceId, kind)]));
  if (instance.componentId === 'agent-manager') {
    const originalRoot = prior.settings.internalProjectRoot;
    if (!internalRoots.has(originalRoot)) internalRoots.set(originalRoot, join(targetRoot, 'codex-projects', instance.instanceId));
    instance.settings.internalProjectRoot = internalRoots.get(originalRoot);
  }
  if (instance.componentId === 'agent-manager' && instance.settings.appServer?.mode === 'owned-stdio') {
    const oldData = servicePaths(source, prior).data;
    const owned = instanceOwnedCodexHome(instance.settings, oldData);
    if (owned) {
      const newHome = join(servicePaths(target, instance).data, relative(oldData, owned));
      if (instance.settings.codexHome !== undefined) instance.settings.codexHome = newHome;
      if (instance.settings.nativeHome !== undefined) instance.settings.nativeHome = newHome;
    }
  }
  if (['host-executor', 'service-manager'].includes(instance.componentId)) instance.settings.hostConfigPath = hostConfigurationPath(target);
  if (instance.componentId === 'hive') {
    instance.settings.listenHost = '127.0.0.1'; instance.settings.listenPort = port;
    instance.settings.backup.directory = join(target.runtimeRoot, 'hive-backups');
  }
}
validateHost('HostConfig', target);
assert.deepEqual(await readFile(configPath), sourceBytes, 'Source configuration changed during planning.');
await privateDirectory(targetRoot);
const templatePath = join(targetRoot, 'restore-template.json'); await atomicJson(templatePath, target);
await atomicJson(join(targetRoot, 'template-report.json'), { schemaVersion: 1, phase: 'prepared_not_restored',
  sourceConfigurationHash: hashJson(source), targetConfigurationHash: hashJson(target), templatePath,
  targetRoots: [target.runtimeRoot, target.artifactRoot, target.stagingRoot], port, instances: target.instances.map(({ instanceId }) => instanceId) });
console.log(JSON.stringify({ phase: 'prepared_not_restored', templatePath }));
