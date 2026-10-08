import { canonical, compareVersions } from '../../../packages/contracts/src/canonical.js';
import { requireThat } from '../../../packages/contracts/src/errors.js';
import { operationId } from '../../../packages/contracts/src/operation-id.js';
import type { DataContract } from '../../../packages/contracts/src/types.js';
import { validatePackageAuthorization, validatePackageCatalog } from '../../../packages/host-runtime/src/package-archive.js';
import type { PackageCatalog, PackageCatalogEntry, PackageUploadAuthorization } from '../../../packages/host-runtime/src/package-archive.js';
import { Objects } from './objects.js';
import { HiveStore } from './store.js';

const contract: DataContract = {
  key: 'ivy/package-catalog', version: '1.0.2', owner: { kind: 'hive' }, mediaType: 'application/json',
  retention: { objects: { mode: 'retain' }, revisions: { mode: 'current' } },
  specMarkdown: 'Validated Ivy package catalog. Retain the latest two archives per component and builds referenced by installation observations or unfinished deployments.',
  jsonSchema: { type: 'object', properties: { schemaVersion: { const: 1 }, revision: { type: 'integer', minimum: 0 }, packages: { type: 'array' } },
    required: ['schemaVersion', 'revision', 'packages'], additionalProperties: false },
};

export interface PackageCatalogSnapshot { objectId: string; objectRevision: number; catalog: PackageCatalog }

export class PackageCatalogObject {
  constructor(readonly store: HiveStore, readonly objects: Objects,
    readonly protectedBuilds: () => ReadonlySet<string> = () => new Set()) { this.objects.register(contract, contract.owner); }

  private objectId(): string | null {
    const rows = this.store.all('SELECT id FROM objects WHERE contract_key=? ORDER BY id LIMIT 2', contract.key);
    requireThat(rows.length <= 1, 'storage_invalid', 'Hive retained more than one package catalog Object.');
    return rows[0] ? String(rows[0]['id']) : null;
  }

  current(): PackageCatalogSnapshot | null {
    const objectId = this.objectId(); if (!objectId) return null;
    const read = this.objects.read({ objectId });
    requireThat(read.content.encoding === 'json', 'storage_invalid', 'Package catalog Object is not JSON.');
    return { objectId, objectRevision: read.revision.revision, catalog: validatePackageCatalog(read.content.value) };
  }

  seed(value: unknown, principalId: string): PackageCatalogSnapshot {
    const existing = this.current(); if (existing) return existing;
    const catalog = validatePackageCatalog(value);
    this.objects.writeHive({ principalId, credentialDigest: 'internal:package-catalog' }, 'packages.catalog.seed', {
      mutationId: operationId(this.store.runtimeEpoch), contractVersion: contract.version, references: {},
      create: { contractKey: contract.key, parentId: null, ownerObjectId: null, name: 'Ivy package catalog' },
      content: { encoding: 'json', value: JSON.parse(canonical(catalog)) },
    });
    return this.current()!;
  }

  list(after: number, includeUis: boolean): PackageCatalogSnapshot {
    requireThat(Number.isSafeInteger(after) && after >= 0, 'invalid_arguments', 'Package catalog revision is invalid.');
    const current = this.current(); requireThat(current, 'service_unavailable', 'Package catalog Object is not initialized.');
    return { ...current, catalog: { schemaVersion: 1, revision: current.catalog.revision,
      packages: current.catalog.packages.filter(item => item.revision > after && (includeUis || item.manifest.kind !== 'app')) } };
  }

  private retainedPackages(packages: PackageCatalogEntry[]): PackageCatalogEntry[] {
    const components = new Map<string, PackageCatalogEntry[]>();
    for (const entry of packages) {
      const versions = components.get(entry.componentId) ?? [];
      versions.push(entry);
      components.set(entry.componentId, versions);
    }
    const keep = new Set<PackageCatalogEntry>();
    for (const versions of components.values())
      for (const entry of versions.sort((a, b) => compareVersions(b.version, a.version)).slice(0, 2)) keep.add(entry);
    const protectedBuilds = this.protectedBuilds();
    return packages.filter(entry => keep.has(entry) || protectedBuilds.has(entry.buildId));
  }

  prune(principalId: string): PackageCatalogEntry[] {
    const current = this.current();
    requireThat(current, 'service_unavailable', 'Package catalog Object is not initialized.');
    const packages = this.retainedPackages(current.catalog.packages);
    if (packages.length === current.catalog.packages.length) return [];
    const removed = current.catalog.packages.filter(entry => !packages.includes(entry));
    const next = validatePackageCatalog({ schemaVersion: 1, revision: current.catalog.revision, packages });
    this.objects.writeHive({ principalId, credentialDigest: 'internal:package-prune' }, 'packages.catalog.prune', {
      mutationId: operationId(this.store.runtimeEpoch), contractVersion: contract.version, references: {},
      objectId: current.objectId, expectedRevision: current.objectRevision,
      content: { encoding: 'json', value: JSON.parse(canonical(next)) },
    });
    return removed;
  }

  publish(input: PackageUploadAuthorization, publisherPrincipalId: string): PackageCatalogEntry {
    const authorized = validatePackageAuthorization(input), current = this.current();
    requireThat(current, 'service_unavailable', 'Package catalog Object is not initialized.');
    const latest = current.catalog.packages.filter(item => item.componentId === authorized.componentId)
      .sort((a, b) => compareVersions(b.version, a.version))[0];
    if (latest && compareVersions(authorized.version, latest.version) <= 0) {
      requireThat(authorized.version === latest.version && authorized.archiveHash === latest.archiveHash && authorized.buildId === latest.buildId &&
        canonical(authorized.manifest) === canonical(latest.manifest), 'version_conflict', 'Published package versions are immutable and must increase.');
      return latest;
    }
    const entry: PackageCatalogEntry = { ...authorized, revision: current.catalog.revision + 1,
      publishedAt: new Date().toISOString(), publisherPrincipalId };
    const next = validatePackageCatalog({ schemaVersion: 1, revision: entry.revision,
      packages: this.retainedPackages([...current.catalog.packages, entry]) });
    this.objects.writeHive({ principalId: publisherPrincipalId, credentialDigest: 'internal:package-upload' }, 'packages.catalog.publish', {
      mutationId: operationId(this.store.runtimeEpoch), contractVersion: contract.version, references: {},
      objectId: current.objectId, expectedRevision: current.objectRevision,
      content: { encoding: 'json', value: JSON.parse(canonical(next)) },
    });
    return entry;
  }
}
