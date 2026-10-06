import { readFile } from 'node:fs/promises';
import { resolve, join } from 'node:path';
import { pathToFileURL } from 'node:url';
import { parseArgs } from 'node:util';
import { setTimeout as delay } from 'node:timers/promises';

const { values } = parseArgs({
  options: {
    distribution: { type: 'string' },
    config: { type: 'string' },
    candidates: { type: 'string' },
    components: { type: 'string' },
    'preserve-enabled-state': { type: 'boolean' },
    'replace-legacy-owner': { type: 'boolean' },
    'operation-prefix': { type: 'string' },
  },
});

for (const key of ['distribution', 'config', 'candidates']) {
  if (!values[key]) throw new Error('Missing --' + key);
}

const load = (path) => import(pathToFileURL(join(resolve(values.distribution), 'dist', path)).href);
const { hostConfig } = await load('packages/host-runtime/src/host-config.js');
const { HostJournal } = await load('packages/host-runtime/src/journal.js');
const { bootstrapPlan, installBootstrap } = await load('packages/host-runtime/src/bootstrap.js');
const { cli } = await load('packages/cli/src/main.js');
const { hashJson } = await load('packages/contracts/src/canonical.js');

const configPath = resolve(values.config);
const config = await hostConfig(configPath);
const candidates = JSON.parse(await readFile(values.candidates, 'utf8'));
const executor = candidates.find((candidate) => candidate.componentId === 'host-executor');
if (!executor) throw new Error('Missing executor candidate');

const configured = values.components
  ? values.components.split(',').filter(Boolean)
  : config.instances.map((instance) => instance.componentId);
const selected = [...new Set(configured)];
const candidateEntries = [];
const journal = new HostJournal(config);
try {
  for (const candidate of candidates) {
    const manifest = JSON.parse(await readFile(candidate.manifestPath, 'utf8'));
    journal.saveCandidate(candidate, manifest);
    candidateEntries.push({ candidate, manifest });
  }
} finally {
  journal.close();
}

const preferredBootstrap = candidateEntries.filter(({ candidate }) => config.instances.some((instance) =>
  instance.engine === 'process' && candidate.componentId === instance.componentId &&
  (candidate.componentId === 'host-executor' || candidate.componentId === 'service-manager' || selected.includes(candidate.componentId))));

// Bootstrap owns HostExecutor and the OS/service owner owns every other
// process. The journal is the durable operation state; this tool keeps no
// second rollout report or temporary supervisor.
const bootstrap = await bootstrapPlan(configPath, executor.candidateId, undefined, preferredBootstrap);
const installation = await installBootstrap(bootstrap, executor.artifactRoot, { replaceLegacy: Boolean(values['replace-legacy-owner']) });
if (!installation.ok) throw new Error('Bootstrap installation failed: ' + installation.code);

const readJson = async (path) => {
  try { return JSON.parse(await readFile(path, 'utf8')); } catch { return null; }
};
const fresh = (value) => Boolean(value && typeof value.observedAt === 'string' && Date.now() - Date.parse(value.observedAt) < 15_000 && Date.parse(value.observedAt) <= Date.now() + 1000);
const waitForBootstrapOwners = async () => {
  const deadline = Date.now() + 120_000;
  while (Date.now() < deadline) {
    const executorStatus = await readJson(join(config.runtimeRoot, 'executor.json'));
    const executorReady = executorStatus?.hostId === config.hostId && executorStatus.state === 'ready' && fresh(executorStatus);
    let serviceManagerReady = true;
    const manager = bootstrap.processes.find((process) => process.componentId === 'service-manager');
    if (manager?.configPath) {
      const instance = await readJson(manager.configPath);
      const health = instance ? await readJson(join(instance.dataRoot, 'health.json')) : null;
      const candidate = candidateEntries.find(({ candidate }) => candidate.candidateId === manager.candidateId)?.candidate;
      serviceManagerReady = Boolean(health?.ready === true && fresh(health) && (!candidate || health.buildId === candidate.buildId));
    }
    if (executorReady && serviceManagerReady) return;
    await delay(500);
  }
  throw new Error('Bootstrap owners did not establish readiness within 120 seconds.');
};
await waitForBootstrapOwners();

const bootstrapOwned = new Set(bootstrap.processes.map((process) => process.componentId));
const order = ['hive', ...selected.filter((component) => !['hive', ...bootstrapOwned].includes(component))]
  .filter((component) => selected.includes(component));
const operationPrefix = values['operation-prefix']
  ?? 'rollout-' + hashJson({
    hostId: config.hostId,
    candidateIds: candidates.map((candidate) => candidate.candidateId).sort(),
  }).slice(7, 23);

for (const component of order) {
  const instance = config.instances.find((value) => value.componentId === component);
  const candidate = candidates.find((value) => value.componentId === component);
  if (!instance || !candidate) throw new Error('Missing configured candidate ' + component);
  const actions = ['deploy'];
  for (const action of actions) {
    const operationId = operationPrefix + '-' + action + '-' + hashJson({
      component,
      instanceId: instance.instanceId,
      candidateId: action === 'deploy' ? candidate.candidateId : null,
    }).slice(7, 23);
    const result = await cli([
      action,
      '--config',
      configPath,
      '--instance',
      instance.instanceId,
      ...(action === 'deploy' ? ['--candidate', candidate.candidateId] : []),
      '--operation-id',
      operationId,
      '--wait-ms',
      '600000',
      '--json',
    ], process.env, (message) => console.error(message));
    const phase = result.output.data?.record?.phase ?? result.output.code;
    console.log(JSON.stringify({ component, action, code: result.output.code, phase }));
    if (result.exitCode !== 0) {
      throw new Error('Deployment failed for ' + component + '; inspect operation ' + operationId);
    }
    if (result.output.deploymentId) {
      const status = await cli([
        'status',
        '--config',
        configPath,
        '--deployment',
        result.output.deploymentId,
        '--json',
      ]);
      const statusPhase = status.output.data?.record?.phase;
      if (status.exitCode !== 0 || statusPhase !== 'succeeded') {
        throw new Error('Activation is not complete for ' + component + ': ' + (statusPhase ?? status.output.code));
      }
    }
  }
  const stateJournal = new HostJournal(config);
  const deployed = stateJournal.installed(instance.instanceId);
  stateJournal.close();
  if (!values['preserve-enabled-state'] && deployed?.enabled !== true) {
    const action = 'enable';
    const operationId = operationPrefix + '-' + action + '-' + hashJson({
      component,
      instanceId: instance.instanceId,
      candidateId: null,
    }).slice(7, 23);
    const enabled = await cli([
      action,
      '--config',
      configPath,
      '--instance',
      instance.instanceId,
      '--operation-id',
      operationId,
      '--wait-ms',
      '600000',
      '--json',
    ], process.env, (message) => console.error(message));
    const phase = enabled.output.data?.record?.phase ?? enabled.output.code;
    console.log(JSON.stringify({ component, action, code: enabled.output.code, phase }));
    if (enabled.exitCode !== 0) throw new Error('Deployment failed for ' + component + '; inspect operation ' + operationId);
  }
}
