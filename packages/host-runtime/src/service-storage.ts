import { DatabaseSync } from 'node:sqlite';
import { existsSync } from 'node:fs';
import { join } from 'node:path';
import { servicePaths } from './layout.js';
import { requireThat } from '../../contracts/src/errors.js';
import type { Host } from '../../contracts/src/generated.js';

// These are Ivy-owned journals, not the provider's private databases.
const journals: Record<string, string> = {
  'phone-bridge': 'phone-commands.sqlite',
};

export function verifyServiceStorage(config: Host.HostConfig, instanceId: string, plan: Pick<Host.BuildPlan, 'componentId' | 'storage'>, verifiedRunningFormat?: number): void {
  const local = journals[plan.componentId];
  if (!local) return;
  requireThat(plan.storage, 'incompatible_storage', 'Service journal requires an explicit storage contract.');
  const path = join(servicePaths(config, instanceId).data, local);
  if (!existsSync(path)) return;
  const db = new DatabaseSync(path, { readOnly: true });
  try {
    db.exec('PRAGMA busy_timeout=100');
    let format: number;
    try { format = Number(db.prepare('PRAGMA user_version').get()!['user_version']); }
    catch (error) {
      // An exclusively owned journal cannot be read concurrently. Only fresh, ready,
      // generation-bound evidence permits using the running binary's exact write format.
      if (![5, 6].includes(Number((error as { errcode?: number }).errcode) & 255) || verifiedRunningFormat === undefined) throw error;
      format = verifiedRunningFormat;
    }
    requireThat(format === 0 || (format >= plan.storage.minReadableFormat && format <= plan.storage.maxReadableFormat && format === plan.storage.writeFormat),
      'incompatible_storage', `${plan.componentId} storage format ${format} cannot be used by candidate format ${plan.storage.writeFormat}. Update refused before activation; retained data requires an explicit development decision.`);
  } finally { db.close(); }
}
