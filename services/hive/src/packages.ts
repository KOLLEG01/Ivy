import { createHash, randomBytes, randomUUID } from 'node:crypto';
import { createWriteStream } from 'node:fs';
import { lstat, mkdir, readFile, readdir, rename, rm, rmdir, writeFile } from 'node:fs/promises';
import { dirname, join, resolve, sep } from 'node:path';
import { Transform } from 'node:stream';
import { pipeline } from 'node:stream/promises';
import type { IncomingMessage } from 'node:http';
import { canonical, compareVersions, digest } from '../../../packages/contracts/src/canonical.js';
import { IvyError, requireThat } from '../../../packages/contracts/src/errors.js';
import { validatePackageArchive, validatePackageAuthorization, validatePackageCatalog } from '../../../packages/host-runtime/src/package-archive.js';
import type { PackageCatalog, PackageCatalogEntry, PackageUploadAuthorization } from '../../../packages/host-runtime/src/package-archive.js';
import type { Host } from '../../../packages/contracts/src/generated.js';
import { fileHash } from '../../../packages/host-runtime/src/artifact.js';
import type { PackageCatalogSnapshot } from './package-catalog-object.js';
import { WorkerClient } from './worker-client.js';

interface UploadGrant extends PackageUploadAuthorization {
  uploadId: string;
  tokenDigest: string;
  principalId: string;
  expiresAt: number;
}
export interface PackageUiInstallation {
  manifest: Host.ReleaseManifest;
  artifactRoot: string;
  publisherPrincipalId: string;
  buildId: string;
}

export class PackageRegistry {
  private readonly grants = new Map<string, UploadGrant>();
  private commitChain = Promise.resolve();

  constructor(readonly root: string, readonly basePath: string, readonly executables: Record<string, string> = {},
    readonly installUi?: (input: PackageUiInstallation) => Promise<void>, readonly worker?: WorkerClient) {}

  async initialize(): Promise<void> {
    requireThat(this.worker, 'service_unavailable', 'Hive package storage worker is unavailable.');
    await mkdir(this.root, { recursive: true });
    let present = true;
    try { await this.worker.request({ action: 'package.catalog', after: 0, includeUis: true }); }
    catch (error) {
      if (!(error instanceof IvyError) || error.code !== 'service_unavailable') throw error;
      present = false;
    }
    if (!present) {
      let catalog: PackageCatalog = { schemaVersion: 1, revision: 0, packages: [] };
      try { catalog = validatePackageCatalog(JSON.parse(await readFile(join(this.root, 'catalog.json'), 'utf8'))); }
      catch (error) { if ((error as NodeJS.ErrnoException).code !== 'ENOENT') throw error; }
      await this.worker.request({ action: 'package.seed', catalog });
    }
    await this.worker.request({ action: 'package.pruneApps' });
    await this.collectAppArtifacts();
    await this.collectIncoming();
  }

  private async collectAppArtifacts(): Promise<void> {
    const catalog = await this.catalog(0);
    const apps = new Map<string, Set<string>>();
    for (const entry of catalog.packages) if (entry.manifest.kind === 'app') {
      const versions = apps.get(entry.componentId) ?? new Set<string>();
      versions.add(entry.version);
      apps.set(entry.componentId, versions);
    }
    const root = resolve(this.root, 'artifacts');
    const directories = await readdir(root, { withFileTypes: true }).catch(error => {
      if ((error as NodeJS.ErrnoException).code === 'ENOENT') return [];
      throw error;
    });
    for (const entry of directories) {
      if (!entry.isDirectory() || !/^[a-z][a-z0-9-]{0,127}$/.test(entry.name)) continue;
      const marker = resolve(root, entry.name, '.app');
      const info = await lstat(marker).catch(error => {
        if ((error as NodeJS.ErrnoException).code === 'ENOENT') return null;
        throw error;
      });
      if (info) {
        requireThat(info.isFile() && !info.isSymbolicLink(), 'storage_invalid', 'App artifact marker is invalid.');
        if (!apps.has(entry.name)) apps.set(entry.name, new Set());
      }
    }
    for (const [appId, versions] of apps) {
      const directory = resolve(root, appId);
      requireThat(directory.startsWith(root + sep), 'storage_invalid', 'App artifact directory is invalid.');
      const entries = await readdir(directory, { withFileTypes: true }).catch(error => {
        if ((error as NodeJS.ErrnoException).code === 'ENOENT') return [];
        throw error;
      });
      for (const entry of entries) {
        if (!entry.isDirectory() || versions.has(entry.name)) continue;
        const target = resolve(directory, entry.name);
        requireThat(target.startsWith(directory + sep), 'storage_invalid', 'App artifact path is invalid.');
        await rm(target, { recursive: true, force: true });
      }
      if (versions.size === 0 && (await readdir(directory)).every(name => name === '.app')) {
        await rm(join(directory, '.app'), { force: true });
        await rmdir(directory);
      }
    }
  }

  private async collectIncoming(now = Date.now()): Promise<void> {
    const directory = resolve(this.root, 'incoming');
    const entries = await readdir(directory, { withFileTypes: true }).catch(error => {
      if ((error as NodeJS.ErrnoException).code === 'ENOENT') return [];
      throw error;
    });
    for (const entry of entries) {
      if (!/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}(?:\.tar\.gz|-extract)$/.test(entry.name)) continue;
      if (!entry.isFile() && !entry.isDirectory()) continue;
      const target = resolve(directory, entry.name);
      requireThat(target.startsWith(directory + sep), 'storage_invalid', 'Incoming package path is invalid.');
      if ((await lstat(target)).mtimeMs > now - 24 * 60 * 60 * 1000) continue;
      await rm(target, { recursive: entry.isDirectory(), force: true });
    }
  }

  async collectArtifacts(): Promise<void> {
    const previous = this.commitChain; let release!: () => void;
    this.commitChain = new Promise<void>(resolve => { release = resolve; });
    await previous;
    try { await this.collectAppArtifacts(); await this.collectIncoming(); }
    finally { release(); }
  }

  private collect(): void {
    const now = Date.now(); for (const [id, grant] of this.grants) if (grant.expiresAt <= now) this.grants.delete(id);
  }

  async authorize(principalId: string, input: unknown): Promise<Record<string, unknown>> {
    this.collect(); const request = validatePackageAuthorization(input);
    const versions = (await this.catalog(0)).packages.filter(item => item.componentId === request.componentId)
      .sort((a, b) => compareVersions(b.version, a.version));
    const latest = versions[0];
    if (latest && compareVersions(request.version, latest.version) <= 0) {
      requireThat(request.version === latest.version && request.archiveHash === latest.archiveHash && request.buildId === latest.buildId &&
        canonical(request.manifest) === canonical(latest.manifest), 'version_conflict', 'Published package versions are immutable and must increase.');
      return { alreadyPublished: true, entry: latest };
    }
    requireThat(this.grants.size < 128, 'capacity_exceeded', 'Too many package uploads are pending.');
    const uploadId = randomUUID(), token = 'ivypu1.' + randomBytes(32).toString('base64url'), expiresAt = Date.now() + 5 * 60_000;
    this.grants.set(uploadId, { ...request, uploadId, tokenDigest: digest(token), principalId, expiresAt });
    return { alreadyPublished: false, uploadId, uploadToken: token,
      uploadPath: `${this.basePath}/api/v1/packages/uploads/${uploadId}`, expiresAt: new Date(expiresAt).toISOString() };
  }

  async catalog(after: number, includeUis = true): Promise<PackageCatalog> {
    return (await this.catalogSnapshot(after, includeUis)).catalog;
  }
  async catalogSnapshot(after: number, includeUis = true): Promise<PackageCatalogSnapshot> {
    requireThat(this.worker, 'service_unavailable', 'Hive package storage worker is unavailable.');
    return this.worker.request<PackageCatalogSnapshot>({ action: 'package.catalog', after, includeUis });
  }

  async artifact(componentId: string, version: string): Promise<{ path: string; entry: PackageCatalogEntry }> {
    const entry = (await this.catalog(0)).packages.find(item => item.componentId === componentId && item.version === version);
    requireThat(entry, 'not_found', 'Package version was not found.');
    const path = join(this.root, 'artifacts', componentId, version, entry.archiveHash.slice('sha256:'.length) + '.tar.gz');
    const metadata = await lstat(path);
    requireThat(metadata.isFile() && !metadata.isSymbolicLink() && metadata.size === entry.bytes, 'artifact_changed', 'Published package artifact is unavailable.');
    return { path, entry };
  }

  async upload(request: IncomingMessage, uploadId: string): Promise<PackageCatalogEntry> {
    this.collect();
    const authorization = request.headers.authorization;
    requireThat(typeof authorization === 'string' && /^Bearer ivypu1\.[A-Za-z0-9_-]+$/.test(authorization), 'unauthenticated', 'Package upload requires its one-time token.');
    const grant = this.grants.get(uploadId);
    requireThat(grant && grant.expiresAt > Date.now() && grant.tokenDigest === digest(authorization.slice(7)), 'unauthenticated', 'Package upload token is invalid or expired.');
    this.grants.delete(uploadId);
    requireThat(request.headers['content-encoding'] === undefined, 'invalid_arguments', 'Package upload content encoding is unsupported.');
    const declared = request.headers['content-length'];
    requireThat(typeof declared === 'string' && /^\d+$/.test(declared) && Number(declared) === grant.bytes, 'invalid_arguments', 'Package upload length differs from its grant.');
    const incoming = join(this.root, 'incoming', uploadId + '.tar.gz'), extraction = join(this.root, 'incoming', uploadId + '-extract');
    await mkdir(dirname(incoming), { recursive: true });
    const hash = createHash('sha256'); let bytes = 0;
    const counter = new Transform({ transform(chunk: Buffer, _encoding, callback) {
      bytes += chunk.length;
      if (bytes > grant.bytes) { callback(new IvyError('content_too_large', 'Package upload exceeds its authorized size.')); return; }
      hash.update(chunk); callback(null, chunk);
    } });
    try {
      request.setTimeout(5 * 60_000);
      await pipeline(request, counter, createWriteStream(incoming, { flags: 'wx' }));
      requireThat(bytes === grant.bytes && 'sha256:' + hash.digest('hex') === grant.archiveHash, 'artifact_changed', 'Package upload checksum differs from its grant.');
      await validatePackageArchive(incoming, grant.archiveHash, grant.manifest, extraction, this.executables);
      let result!: PackageCatalogEntry;
      const commit = async () => {
        const latest = (await this.catalog(0)).packages.filter(item => item.componentId === grant.componentId).sort((a, b) => compareVersions(b.version, a.version))[0];
        if (latest && compareVersions(grant.version, latest.version) <= 0) {
          requireThat(grant.version === latest.version && grant.archiveHash === latest.archiveHash && grant.buildId === latest.buildId,
            'version_conflict', 'Published package versions are immutable and must increase.');
          result = latest; return;
        }
        if (grant.manifest.kind === 'app') await this.installUi?.({ manifest: grant.manifest, artifactRoot: join(extraction, 'artifact'),
          publisherPrincipalId: grant.principalId, buildId: grant.buildId });
        requireThat(grant.manifest.kind !== 'app' || this.installUi, 'service_unavailable', 'Hive has no app package installer.');
        const target = join(this.root, 'artifacts', grant.componentId, grant.version, grant.archiveHash.slice(7) + '.tar.gz');
        await mkdir(dirname(target), { recursive: true });
        if (grant.manifest.kind === 'app') {
          const marker = join(this.root, 'artifacts', grant.componentId, '.app');
          await writeFile(marker, '', { flag: 'wx' }).catch(error => {
            if ((error as NodeJS.ErrnoException).code !== 'EEXIST') throw error;
          });
          const info = await lstat(marker);
          requireThat(info.isFile() && !info.isSymbolicLink(), 'storage_invalid', 'App artifact marker is invalid.');
        }
        const retained = await lstat(target).catch(error => {
          if ((error as NodeJS.ErrnoException).code === 'ENOENT') return null;
          throw error;
        });
        if (retained) {
          requireThat(retained.isFile() && !retained.isSymbolicLink() && retained.size === grant.bytes &&
            await fileHash(target) === grant.archiveHash, 'artifact_changed', 'Retained package archive differs from the authorized upload.');
        } else await rename(incoming, target);
        const { uploadId: _uploadId, tokenDigest: _tokenDigest, principalId: _principalId, expiresAt: _expiresAt, ...entry } = grant;
        requireThat(this.worker, 'service_unavailable', 'Hive package storage worker is unavailable.');
        result = await this.worker.request<PackageCatalogEntry>({ action: 'package.publish', input: entry, publisherPrincipalId: grant.principalId });
        await this.collectAppArtifacts();
      };
      const previous = this.commitChain; let release!: () => void; this.commitChain = new Promise<void>(resolve => { release = resolve; });
      await previous;
      try { await commit(); }
      catch (error) {
        if (grant.manifest.kind === 'app') await this.collectAppArtifacts().catch(() => undefined);
        throw error;
      } finally { release(); }
      return result;
    } finally { await rm(incoming, { force: true }).catch(() => undefined); await rm(extraction, { recursive: true, force: true }).catch(() => undefined); }
  }
}
