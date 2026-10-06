import { lstat, mkdir, readFile, realpath, rm } from 'node:fs/promises';
import { dirname, join, resolve } from 'node:path';
import { canonical, compareVersions } from '../../contracts/src/canonical.js';
import { requireThat } from '../../contracts/src/errors.js';
import { validateHost } from '../../contracts/src/host-validation.js';
import type { Host } from '../../contracts/src/generated.js';
import { artifactFiles, fileHash, readReleaseManifest, verifyTransferredArchive } from './artifact.js';
import { inside, jsonFile } from './config.js';
import { runCommand } from './process.js';
import { portableNativeFiles } from './portable-native-files.js';

export interface PackageCatalogEntry {
  componentId: string;
  version: string;
  buildId: string;
  archiveHash: string;
  bytes: number;
  manifest: Host.ReleaseManifest;
  revision: number;
  publishedAt: string;
  publisherPrincipalId: string;
}
export interface PackageCatalog {
  schemaVersion: 1;
  revision: number;
  packages: PackageCatalogEntry[];
}
export interface PackageCatalogIndex {
  schemaVersion: 1;
  revision: number;
  packages: Array<{ componentId: string; version: string }>;
}
export interface PackageUploadAuthorization {
  componentId: string;
  version: string;
  buildId: string;
  archiveHash: string;
  bytes: number;
  manifest: Host.ReleaseManifest;
}

const hash = (value: unknown): value is string => typeof value === 'string' && /^sha256:[0-9a-f]{64}$/.test(value);
const id = (value: unknown): value is string => typeof value === 'string' && /^[a-z][a-z0-9-]{0,127}$/.test(value);
const archiveExecutables = (executables: Record<string, string>): Record<string, string> => ({ ...executables,
  tar: executables['tar'] ?? (process.platform === 'win32' ? join(process.env['SystemRoot'] ?? 'C:\\Windows', 'System32', 'tar.exe') : '/usr/bin/tar') });

export function validatePackageAuthorization(value: unknown): PackageUploadAuthorization {
  requireThat(value !== null && typeof value === 'object' && !Array.isArray(value), 'invalid_arguments', 'Package upload authorization needs an object.');
  const input = value as Partial<PackageUploadAuthorization>;
  validateHost('ReleaseManifest', input.manifest);
  const manifest = input.manifest as Host.ReleaseManifest;
  compareVersions(String(input.version), '0.0.0');
  requireThat(id(input.componentId) && input.componentId === manifest.componentId && input.version === manifest.version &&
    hash(input.buildId) && input.buildId === manifest.buildId && hash(input.archiveHash) &&
    Number.isSafeInteger(input.bytes) && Number(input.bytes) > 0 && Number(input.bytes) <= 256 * 1024 * 1024,
  'invalid_arguments', 'Package upload identity does not match its manifest.');
  return { componentId: input.componentId, version: input.version, buildId: input.buildId,
    archiveHash: input.archiveHash, bytes: Number(input.bytes), manifest };
}

/** Read only stable catalog identity fields before selecting a bootstrap update.
 * A newer release manifest for another component must not block that update. */
export function validatePackageCatalogIndex(value: unknown): PackageCatalogIndex {
  requireThat(value !== null && typeof value === 'object' && !Array.isArray(value), 'invalid_arguments', 'Package catalog needs an object.');
  const catalog = value as Partial<PackageCatalog>;
  requireThat(catalog.schemaVersion === 1 && Number.isSafeInteger(catalog.revision) && Number(catalog.revision) >= 0 &&
    Array.isArray(catalog.packages) && catalog.packages.length <= 4096, 'invalid_arguments', 'Package catalog is invalid.');
  for (const entry of catalog.packages) {
    requireThat(entry !== null && typeof entry === 'object' && !Array.isArray(entry) && id(entry.componentId) &&
      typeof entry.version === 'string', 'invalid_arguments', 'Package catalog entry identity is invalid.');
    compareVersions(entry.version, '0.0.0');
  }
  return catalog as PackageCatalogIndex;
}

export function validatePackageCatalog(value: unknown): PackageCatalog {
  const catalog = validatePackageCatalogIndex(value) as PackageCatalog;
  for (const entry of catalog.packages) {
    const authorized = validatePackageAuthorization(entry);
    requireThat(Number.isSafeInteger(entry.revision) && entry.revision > 0 && typeof entry.publishedAt === 'string' && Number.isFinite(Date.parse(entry.publishedAt)) &&
      typeof entry.publisherPrincipalId === 'string' && entry.publisherPrincipalId.length > 0 && entry.publisherPrincipalId.length <= 256 &&
      canonical(authorized.manifest) === canonical(entry.manifest), 'invalid_arguments', 'Package catalog entry is invalid.');
  }
  return catalog as PackageCatalog;
}

function safeArchiveEntry(raw: string): string {
  const value = raw.replace(/^\.\//, '').replace(/\/$/, '');
  requireThat(value.length > 0 && value.length <= 2048 && !value.includes('\\') && !value.startsWith('/') &&
    value.split('/').every(part => part.length > 0 && part !== '.' && part !== '..'), 'artifact_changed', 'Package archive contains an unsafe path.');
  requireThat(value === 'component.json' || value === 'artifact' || value.startsWith('artifact/'), 'artifact_changed', 'Package archive contains an unexpected root.');
  return value;
}

export async function createPackageArchive(candidate: Host.Candidate, executables: Record<string, string>): Promise<{ path: string; hash: string; bytes: number }> {
  const root = dirname(candidate.artifactRoot), destination = join(root, 'package.tar.gz');
  const result = await runCommand({ executable: 'tar', args: ['-czf', destination, '-C', root, 'artifact', 'component.json'], timeoutMs: 180_000 }, root, archiveExecutables(executables), { useJobLauncher: false });
  requireThat(!result.truncated, 'artifact_changed', 'Package archive command output was truncated.');
  const metadata = await lstat(destination);
  requireThat(metadata.isFile() && !metadata.isSymbolicLink() && metadata.size > 0 && metadata.size <= 256 * 1024 * 1024,
    'artifact_changed', 'Package archive size is invalid.');
  return { path: destination, hash: await fileHash(destination), bytes: metadata.size };
}

export async function validatePackageArchive(archive: string, expectedHash: string, expectedManifest: Host.ReleaseManifest,
  extractionRoot: string, executables: Record<string, string>): Promise<void> {
  await verifyTransferredArchive(archive, expectedHash);
  const listing = await runCommand({ executable: 'tar', args: ['-tzf', archive], timeoutMs: 60_000 }, dirname(archive), archiveExecutables(executables),
    { useJobLauncher: false, maxOutputBytes: 8 * 1024 * 1024 });
  requireThat(!listing.truncated, 'limit_exceeded', 'Package archive file list is too large.');
  const entries = listing.stdout.split(/\r?\n/).filter(Boolean).map(safeArchiveEntry);
  requireThat(entries.length > 1 && entries.length <= 50_000 && entries.includes('component.json') && entries.some(value => value === 'artifact/dist/build-info.json'),
    'artifact_changed', 'Package archive is incomplete.');
  for (const file of portableNativeFiles(expectedManifest.componentId))
    requireThat(entries.includes('artifact/' + file), 'artifact_missing', 'Portable package is missing its Windows runtime payload.');
  await rm(extractionRoot, { recursive: true, force: true }); await mkdir(extractionRoot, { recursive: true });
  await runCommand({ executable: 'tar', args: ['-xzf', archive, '-C', extractionRoot], timeoutMs: 180_000 }, extractionRoot, archiveExecutables(executables), { useJobLauncher: false });
  const actualRoot = await realpath(extractionRoot), artifactRoot = await realpath(join(extractionRoot, 'artifact'));
  requireThat(inside(actualRoot, artifactRoot), 'artifact_changed', 'Package artifact leaves its extraction root.');
  const componentPath = join(extractionRoot, 'component.json'), componentMetadata = await lstat(componentPath);
  requireThat(componentMetadata.isFile() && !componentMetadata.isSymbolicLink(), 'artifact_changed', 'Package manifest is not a regular file.');
  const manifest = await readReleaseManifest(componentPath);
  requireThat(canonical(manifest) === canonical(expectedManifest), 'build_mismatch', 'Package manifest differs from its authorized identity.');
  for (const file of portableNativeFiles(manifest.componentId)) {
    const path = join(artifactRoot, ...file.split('/')), metadata = await lstat(path);
    requireThat(metadata.isFile() && !metadata.isSymbolicLink() && metadata.size > 0 && inside(artifactRoot, await realpath(path)),
      'artifact_changed', 'Portable Windows runtime payload is invalid.');
  }
  const files = await artifactFiles(artifactRoot);
  requireThat(files.length > 0, 'artifact_changed', 'Package artifact is empty.');
  const info = await jsonFile<{ schemaVersion: number; componentId: string; version: string; buildId: string }>(join(artifactRoot, 'dist', 'build-info.json'));
  requireThat(info.schemaVersion === 1 && info.componentId === manifest.componentId && info.version === manifest.version && info.buildId === manifest.buildId,
    'build_mismatch', 'Package build stamp differs from its manifest.');
  for (const command of manifest.kind === 'app' ? [] : [manifest.entrypoint!, manifest.readiness!.command]) {
    const selected = command.args[0];
    if (command.executable === 'node' && selected?.startsWith('dist/')) {
      const path = resolve(artifactRoot, selected), metadata = await lstat(path);
      requireThat(inside(artifactRoot, path) && metadata.isFile() && !metadata.isSymbolicLink(), 'artifact_changed', 'Package launch entry is unavailable.');
    }
  }
  if (manifest.kind === 'app') {
    requireThat(manifest.app && manifest.app.appId === manifest.componentId, 'build_mismatch', 'App package identity differs from its component.');
    const appRoot = resolve(artifactRoot, manifest.app.dist), appMetadata = await lstat(appRoot);
    const entry = resolve(appRoot, manifest.app.entryPath), entryMetadata = await lstat(entry);
    requireThat(inside(artifactRoot, appRoot) && appMetadata.isDirectory() && !appMetadata.isSymbolicLink() &&
      inside(appRoot, entry) && entryMetadata.isFile() && !entryMetadata.isSymbolicLink(), 'artifact_changed', 'UI package bundle is incomplete.');
  }
}
