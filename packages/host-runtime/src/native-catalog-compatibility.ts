import { readdir, readFile } from 'node:fs/promises';
import { join } from 'node:path';
import { BoundToolClient } from '../../sdk/src/client.js';
import type { RpcClient } from '../../sdk/src/client.js';
import { hashJson } from '../../contracts/src/canonical.js';
import { requireThat } from '../../contracts/src/errors.js';
import { validateAgent } from '../../contracts/src/agent-validation.js';
import { nativeCatalogPath } from '../../contracts/src/checked-native-contract.js';
import type { Agent } from '../../contracts/src/generated.js';
import { mapConcurrent } from './concurrency.js';

type CatalogIdentity = Pick<Agent.Status, 'nativeVersion' | 'nativeExecutableHash' | 'catalogHash'>;
const retained = new Map<string, Promise<ReadonlyMap<string, CatalogIdentity>>>();

/** Catch a pinned executable/catalog mismatch before replacing the native owner's process. */
export async function verifyConfiguredNativeCatalog(artifactRoot: string, instanceId: string,
  settings: Pick<Agent.Settings, 'nativeVersion' | 'nativeExecutableHash'>): Promise<void> {
  const value: unknown = JSON.parse(await readFile(nativeCatalogPath(join(artifactRoot, 'dist'), settings.nativeVersion), 'utf8'));
  validateAgent('Catalog', value); const catalog = value as Agent.Catalog;
  requireThat(catalog.version === settings.nativeVersion && catalog.nativeExecutableHash === settings.nativeExecutableHash,
    'incompatible_native_catalog', `${instanceId} is configured for native version ${settings.nativeVersion}, executable ${settings.nativeExecutableHash}, but the candidate requires executable ${catalog.nativeExecutableHash}. Prepare the matching native runtime and configuration before retrying.`);
}

/** Prepared candidates are immutable; retain only their small catalog identity index. */
async function candidateCatalogs(artifactRoot: string): Promise<ReadonlyMap<string, CatalogIdentity>> {
  let pending = retained.get(artifactRoot);
  if (!pending) {
    pending = (async () => {
      const root = join(artifactRoot, 'dist', 'specs', 'native'), catalogs = new Map<string, CatalogIdentity>();
      for (const directory of await readdir(root, { withFileTypes: true })) {
        if (!directory.isDirectory() || !/^codex-[0-9]+\.[0-9]+\.[0-9]+$/.test(directory.name)) continue;
        for (const name of await readdir(join(root, directory.name))) {
          if (!/^catalog(?:\.[a-z0-9]+-[a-z0-9]+)?\.json$/.test(name)) continue;
          const value: unknown = JSON.parse(await readFile(join(root, directory.name, name), 'utf8'));
          validateAgent('Catalog', value);
          const catalog = value as Agent.Catalog, catalogHash = hashJson(catalog);
          catalogs.set(catalogHash, { nativeVersion: catalog.version, nativeExecutableHash: catalog.nativeExecutableHash, catalogHash });
        }
      }
      requireThat(catalogs.size > 0, 'incompatible_native_catalog', 'The candidate declares native catalog consumption but contains no retained catalogs.');
      return catalogs;
    })();
    retained.set(artifactRoot, pending);
    pending.catch(() => { if (retained.get(artifactRoot) === pending) retained.delete(artifactRoot); });
  }
  return pending;
}

/** Read metadata only, before draining the current service. Offline hosts do not block deployment. */
export async function verifyNativeCatalogCompatibility(client: RpcClient, artifactRoot: string, componentId: string): Promise<void> {
  const catalogs = await candidateCatalogs(artifactRoot);
  let cursor: string | undefined;
  const visited = new Set<string>();
  do {
    const page = await client.request('serviceNodes.list', { serviceName: 'agent-manager', limit: 200, ...(cursor ? { cursor } : {}) }, { timeoutMs: 5000 });
    await mapConcurrent(page.items.filter(node => node.connected && node.synced && node.ready && node.desiredEnabled && !node.stale), 8, async node => {
      const tools = new BoundToolClient(client, node.serviceNodeId, [{ namespace: 'agent', interfaceVersion: '1.0.0' }]);
      const value = await tools.read('agent.status', {}, { timeoutMs: 5000 });
      validateAgent('Status', value); const status = value as Agent.Status;
      requireThat(status.serviceNodeId === node.serviceNodeId && status.hostId === node.hostId && status.state === 'ready',
        'compatibility_unknown', `Native owner ${node.serviceNodeId} changed during the ${componentId} deployment check. Retry with its current identity.`);
      const selected = catalogs.get(status.catalogHash);
      requireThat(selected?.nativeVersion === status.nativeVersion && selected.nativeExecutableHash === status.nativeExecutableHash,
        'incompatible_native_catalog', `${componentId} cannot consume the running catalog of ${node.serviceNodeId}: native version ${status.nativeVersion}, catalog ${status.catalogHash}, executable ${status.nativeExecutableHash}. Deploy a matching native owner and configuration before retrying this candidate.`);
    });
    cursor = page.nextCursor ?? undefined;
    if (cursor) { requireThat(!visited.has(cursor), 'compatibility_unknown', 'Native owner discovery returned a repeated cursor.'); visited.add(cursor); }
  } while (cursor);
}
