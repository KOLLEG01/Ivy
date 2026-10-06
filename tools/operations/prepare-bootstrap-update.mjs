import { resolve, join } from 'node:path';
import { pathToFileURL } from 'node:url';
import { writeFile } from 'node:fs/promises';

const [configPath, distribution, candidateId, operationId, outputPath, serviceManagerId] = process.argv.slice(2);
if (!configPath || !distribution || !candidateId || !operationId || !outputPath)
  throw new Error('Usage: prepare-bootstrap-update.mjs CONFIG DISTRIBUTION HOST_CANDIDATE OPERATION_ID OUTPUT [SERVICE_MANAGER_CANDIDATE]');
const moduleAt = name => import(pathToFileURL(join(resolve(distribution), 'dist/packages/host-runtime/src', name + '.js')).href);
const [{ hostConfig }, { HostJournal }, { inspectBootstrap }, { configurationBootstrapPlan, bootstrapComponents }] = await Promise.all([
  moduleAt('host-config'), moduleAt('journal'), moduleAt('bootstrap-maintenance'), moduleAt('bootstrap'),
]);
const config = await hostConfig(resolve(configPath));
const journal = new HostJournal(config);
try {
  const candidate = journal.candidate(candidateId);
  if (candidate.componentId !== 'host-executor') throw new Error('Candidate is not HostExecutor');
  const preferred = serviceManagerId ? (() => {
    const serviceCandidate = journal.candidate(serviceManagerId);
    if (serviceCandidate.componentId !== 'service-manager') throw new Error('Candidate is not ServiceManager');
    return [{ candidate: serviceCandidate, manifest: journal.manifest(serviceManagerId) }];
  })() : [];
  const previous = await inspectBootstrap(resolve(configPath), resolve(distribution));
  const next = configurationBootstrapPlan(config, resolve(configPath), candidate, undefined,
    bootstrapComponents(config, journal, candidate, preferred));
  const request = { schemaVersion: 1, operationId, previous, next };
  await writeFile(resolve(outputPath), JSON.stringify(request, null, 2) + '\n', { flag: 'wx', mode: 0o600 });
  console.log(JSON.stringify({ hostId: config.hostId, operationId, outputPath: resolve(outputPath),
    previous: previous.owners.map(({ instanceId, candidateId }) => ({ instanceId, candidateId })),
    next: next.processes.map(({ instanceId, candidateId }) => ({ instanceId, candidateId })) }));
} finally { journal.close(); }
