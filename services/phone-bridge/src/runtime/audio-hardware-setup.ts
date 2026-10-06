import { canonical, digest } from '../../../../packages/sdk/src/node.js';
import { requireThat } from '../../../../packages/sdk/src/node.js';
import { jsonFile } from '../../../../packages/host-runtime/src/config.js';
import { lstat, open, writeFile } from 'node:fs/promises';
import { isAbsolute } from 'node:path';

export interface AudioHardwareTarget { instanceId: string; name: string; hardwareIds: string[]; disabled: boolean }
export interface AudioHardwareRequest { operationId: string; instanceIds: string[]; receiptPath: string; apply?: boolean }
export interface AudioHardwarePorts {
  readiness(): Promise<{ busy: boolean; ready: boolean; endpointIds: string[] }>;
  inspect(instanceIds: string[], protectedEndpointIds: string[]): Promise<AudioHardwareTarget[]>;
  disable(target: AudioHardwareTarget, protectedEndpointIds: string[]): Promise<void>;
}
/** Explicit setup outside ordinary service startup. Original plans precede any hardware effect. */
export async function setupAudioHardware(request: AudioHardwareRequest, ports: AudioHardwarePorts) {
  requireThat(typeof request.operationId === 'string' && /^[0-9a-f]{8}(-[0-9a-f]{4}){3}-[0-9a-f]{12}$/i.test(request.operationId) && typeof request.receiptPath === 'string' && isAbsolute(request.receiptPath) &&
    (request.apply === undefined || typeof request.apply === 'boolean') &&
    Array.isArray(request.instanceIds) && request.instanceIds.length > 0 && request.instanceIds.length <= 16 &&
    request.instanceIds.every(id => typeof id === 'string' && id.length > 0 && id.length <= 1024 && !/[\x00-\x1f]/.test(id)) &&
    new Set(request.instanceIds.map(id => id.toUpperCase())).size === request.instanceIds.length,
  'invalid_arguments', 'Bounded exact hardware IDs, operation ID and an absolute receipt path are required.');
  const requestHash = digest(canonical(request));
  try {
    const previous = await jsonFile<{ requestHash: string; state: string }>(request.receiptPath, 65536);
    requireThat(previous.requestHash === requestHash, 'mutation_conflict', 'Original hardware setup request changed.');
    try {
      const result = await jsonFile<{ requestHash: string; state: string }>(request.receiptPath + '.result', 65536);
      requireThat(result.requestHash === requestHash, 'mutation_conflict', 'Original hardware result changed.');
      return result;
    } catch (error) { if ((error as NodeJS.ErrnoException).code !== 'ENOENT') throw error; }
    return previous; // Never repeat an accepted/uncertain hardware batch.
  } catch (error) { if ((error as NodeJS.ErrnoException).code !== 'ENOENT') throw error; }
  try {
    await lstat(request.receiptPath + '.result');
    requireThat(false, 'mutation_conflict', 'Hardware result exists without its original accepted request.');
  } catch (error) { if ((error as NodeJS.ErrnoException).code !== 'ENOENT') throw error; }
  const readiness = await ports.readiness();
  requireThat(readiness.ready && !readiness.busy && readiness.endpointIds.length > 0, 'phone_audio_not_ready', 'Configured Phone routes must be idle and ready before hardware changes.');
  const targets = await ports.inspect(request.instanceIds, readiness.endpointIds);
  requireThat(targets.length === request.instanceIds.length && targets.every((target, index) => target.instanceId === request.instanceIds[index]),
    'phone_hardware_changed', 'Exact original hardware targets required.');
  const planned = { schemaVersion: 1, operationId: request.operationId, requestHash, state: request.apply ? 'accepted' : 'planned',
    targets, protectedEndpointIds: readiness.endpointIds, disabledIds: [] as string[] };
  if (!request.apply) return planned;
  requireThat(Buffer.byteLength(JSON.stringify(planned)) <= 60000, 'invalid_arguments', 'Hardware receipt exceeds its bound.');
  const receipt = await open(request.receiptPath, 'wx', 0o600);
  try { await receipt.writeFile(JSON.stringify(planned)); await receipt.sync(); } finally { await receipt.close(); }
  const disabledIds: string[] = [];
  try {
    for (const target of targets) {
      const current = await ports.readiness();
      requireThat(current.ready && !current.busy && canonical(current.endpointIds) === canonical(readiness.endpointIds),
        'phone_audio_not_ready', 'Phone route changed or became busy before hardware mutation.');
      if (!target.disabled) await ports.disable(target, current.endpointIds);
      disabledIds.push(target.instanceId);
    }
    const result = { ...planned, state: 'disabled', disabledIds };
    // The original accepted file remains a fence if final evidence cannot be written.
    await writeFile(request.receiptPath + '.result', JSON.stringify(result), { flag: 'wx', mode: 0o600 });
    return result;
  } catch {
    const result = { ...planned, state: 'outcome_unknown', disabledIds };
    await writeFile(request.receiptPath + '.result', JSON.stringify(result), { flag: 'wx', mode: 0o600 });
    return result;
  }
}
