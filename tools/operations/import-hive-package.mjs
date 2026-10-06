import { resolve, join } from 'node:path';
import { pathToFileURL } from 'node:url';

const [configPath, distribution, componentId, version] = process.argv.slice(2);
if (!configPath || !distribution || !componentId || !version) throw new Error('Usage: import-hive-package.mjs CONFIG DISTRIBUTION COMPONENT VERSION');
const root = resolve(distribution);
const moduleAt = name => import(pathToFileURL(join(root, 'dist/packages/host-runtime/src', name + '.js')).href);
const [{ hostConfig }, { HostJournal }, { PackageUpdater }, { validatePackageCatalog }] = await Promise.all([
  moduleAt('host-config'), moduleAt('journal'), moduleAt('package-updater'), moduleAt('package-archive'),
]);
const config = await hostConfig(resolve(configPath));
const credential = config.instances.find(value => value.componentId === 'host-executor')?.credential;
if (!credential) throw new Error('The owning HostExecutor has no Hive credential.');
const endpoint = new URL(config.publicBaseUrl);
endpoint.pathname = endpoint.pathname.replace(/\/$/, '') + '/api/v1/packages/catalog';
endpoint.searchParams.set('after', '0');
const response = await fetch(endpoint, { headers: { Authorization: 'Bearer ' + credential }, signal: AbortSignal.timeout(10_000) });
if (!response.ok) throw new Error('Hive catalog is unavailable: HTTP ' + response.status);
const catalog = validatePackageCatalog(await response.json());
const entry = catalog.packages.find(value => value.componentId === componentId && value.version === version);
if (!entry) throw new Error('Exact published package is unavailable.');
const journal = new HostJournal(config);
try {
  const candidate = await new PackageUpdater(config, journal, credential).importPackage(entry);
  console.log(JSON.stringify({ hostId: config.hostId, componentId, version, candidateId: candidate.candidateId, buildId: candidate.buildId }));
} finally { journal.close(); }
