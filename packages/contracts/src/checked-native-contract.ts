import { readFileSync, existsSync, readdirSync } from 'node:fs';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { requireThat } from './errors.js';
import { NativeContract } from './native-contract.js';
import { nativeVersions } from './native-versions.js';
import type { Agent, Wire } from './generated.js';

interface CheckedNativeContract {
  catalog: Agent.Catalog;
  catalogHash: string;
  definitions: ReadonlyMap<string, Wire.ToolDefinition>;
  contract: NativeContract;
}
const catalogs = new Map<string, CheckedNativeContract>();
export function nativeCatalogPath(root: string, version: string, platform: NodeJS.Platform = process.platform, architecture: string = process.arch): string {
  requireThat(nativeVersions.includes(version) && /^[a-z0-9]+$/.test(platform) && /^[a-z0-9]+$/.test(architecture), 'native_version_unsupported', 'Supported native platform and version required.');
  const directory = join(root, 'specs', 'native', 'codex-' + version), selected = join(directory, 'catalog.' + platform + '-' + architecture + '.json');
  return existsSync(selected) ? selected : join(directory, 'catalog.json');
}

/** Offline evidence uses the exact retained catalog; live admission uses the SDK owner snapshot. */
export function checkedNativeContract(version: string, identity?: { sourceHash?: string; catalogHash?: string; nativeExecutableHash?: string }): CheckedNativeContract {
  requireThat(nativeVersions.includes(version), 'native_version_unsupported', 'A supported native catalog version is required.');
  const key = version + ':' + JSON.stringify(identity ?? null);
  const existing = catalogs.get(key); if (existing) return existing;
  if (identity !== undefined) {
    requireThat(Object.keys(identity).length > 0 && Object.keys(identity).every(key => ['sourceHash', 'catalogHash', 'nativeExecutableHash'].includes(key)) && Object.values(identity).every(value => typeof value === 'string' && /^sha256:[a-f0-9]{64}$/.test(value)),
      'native_catalog_mismatch', 'An explicit native catalog identity must contain exact hashes.');
    const directory = fileURLToPath(new URL('../../../specs/native/codex-' + version + '/', import.meta.url));
    let files: string[];
    try { files = readdirSync(directory).filter(name => /^catalog(?:\.[a-z0-9]+-[a-z0-9]+)?\.json$/.test(name)); }
    catch { requireThat(false, 'native_version_unsupported', 'The requested native catalog is not retained in this distribution.'); }
    for (const file of files!) {
      const contract = new NativeContract(JSON.parse(readFileSync(join(directory, file), 'utf8')) as Agent.Catalog);
      if (contract.catalog.version !== version || identity.sourceHash !== undefined && contract.catalog.sourceHash !== identity.sourceHash ||
        identity.catalogHash !== undefined && contract.catalogHash !== identity.catalogHash ||
        identity.nativeExecutableHash !== undefined && contract.catalog.nativeExecutableHash !== identity.nativeExecutableHash) continue;
      const selected = Object.freeze({ catalog: contract.catalog, catalogHash: contract.catalogHash, definitions: contract.definitions(), contract });
      catalogs.set(key, selected); return selected;
    }
    requireThat(false, 'native_catalog_mismatch', 'No retained native platform build matches the explicitly selected catalog identity.');
  }
  let source: string;
  try {
    const selected = new URL('../../../specs/native/codex-' + version + '/catalog.' + process.platform + '-' + process.arch + '.json', import.meta.url);
    source = readFileSync(existsSync(selected) ? selected : new URL('../../../specs/native/codex-' + version + '/catalog.json', import.meta.url), 'utf8');
  }
  catch (error) {
    if ((error as NodeJS.ErrnoException).code !== 'ENOENT') throw error;
    requireThat(false, 'native_version_unsupported', 'The requested native catalog is not retained in this distribution.');
  }
  const contract = new NativeContract(JSON.parse(source!) as Agent.Catalog);
  requireThat(contract.catalog.version === version, 'native_catalog_mismatch', 'The retained catalog identifies another native version.');
  const value = Object.freeze({ catalog: contract.catalog, catalogHash: contract.catalogHash, definitions: contract.definitions(), contract });
  catalogs.set(key, value); return value;
}
