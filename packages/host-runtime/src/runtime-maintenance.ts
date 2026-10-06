import { lstat } from 'node:fs/promises';
import { dirname, join } from 'node:path';
import type { Host } from '../../contracts/src/generated.js';

export const runtimeResetMarker = (config: Pick<Host.HostConfig,'ivyRoot'|'runtimeRoot'>) =>
  join(config.ivyRoot ?? dirname(config.runtimeRoot), 'reset', 'maintenance.json');

export const hiveResetBootstrapMarker = (dataRoot: string) => join(dataRoot, 'runtime-reset-bootstrap.json');

export async function runtimeResetActive(config: Pick<Host.HostConfig,'ivyRoot'|'runtimeRoot'>): Promise<boolean> {
  try { return (await lstat(runtimeResetMarker(config))).isFile(); }
  catch (error) { if ((error as NodeJS.ErrnoException).code === 'ENOENT') return false; throw error; }
}
