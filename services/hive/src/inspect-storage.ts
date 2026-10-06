import { DatabaseSync } from 'node:sqlite';
import { existsSync } from 'node:fs';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { validateHost } from '../../../packages/contracts/src/host-validation.js';
import { IvyError, requireThat } from '../../../packages/contracts/src/errors.js';
import type { Host } from '../../../packages/contracts/src/generated.js';
import { configurationPath, instanceConfig } from '../../../packages/host-runtime/src/config.js';
import { STORAGE_FORMAT } from './storage-schema.js';
import { contractVersions } from './contract-usage.js';
import type { ContractRead } from './contract-usage.js';

export function storageInspection(db: DatabaseSync, requirements: ContractRead[]): Host.StorageInspection {
  const keys = requirements.map(value => value.key);
  requireThat(keys.length <= 128 && new Set(keys).size === keys.length && keys.every(key => typeof key === 'string' && key.length > 0 && key.length <= 192), 'invalid_arguments', 'Storage inspection needs bounded distinct contract keys.');
  const format = Number(db.prepare('PRAGMA user_version').get()!['user_version']);
  requireThat(format === STORAGE_FORMAT, 'unsupported_storage_format', 'This Hive inspector does not understand the stored format.');
  const contracts = requirements.map(required => ({ key: required.key, versions: contractVersions(db, required) }));
  const result: Host.StorageInspection = { schemaVersion: 1, exists: true, format, contracts }; validateHost('StorageInspection', result); return result;
}
export function inspectStoppedStorage(filename: string, requirements: ContractRead[]): Host.StorageInspection {
  if (!existsSync(filename)) return { schemaVersion: 1, exists: false, format: 0, contracts: requirements.map(({ key }) => ({ key, versions: [] })) };
  const db = new DatabaseSync(filename, { readOnly: true, timeout: 1000 });
  try { db.exec('PRAGMA query_only=ON; BEGIN'); return storageInspection(db, requirements); }
  finally { if (db.isTransaction) db.exec('ROLLBACK'); db.close(); }
}
if (process.argv[1] && fileURLToPath(import.meta.url) === process.argv[1]) {
  try {
    const config = await instanceConfig(configurationPath());
    requireThat(config.componentId === 'hive', 'target_conflict', 'Only the Hive artifact may inspect its own storage.');
    const args = process.argv.slice(2), position = args.indexOf('--contracts');
    const contracts = position >= 0 ? JSON.parse(args[position + 1]!) as ContractRead[] : [];
    requireThat(Array.isArray(contracts), 'invalid_arguments', 'Contract read domains must be a JSON array.');
    const result = inspectStoppedStorage(join(config.dataRoot, 'hive.sqlite'), contracts); validateHost('StorageInspection', result);
    process.stdout.write(JSON.stringify(result) + '\n');
  } catch (error) { process.stderr.write(JSON.stringify({ code: IvyError.from(error).code, message: 'Hive storage inspection failed without mutation.' }) + '\n'); process.exitCode = 1; }
}
