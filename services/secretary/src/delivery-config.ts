import { IvyError } from '../../../packages/sdk/src/node.js';
import { need } from './schema.js';
import { loadServiceConfiguration } from './service-config.js';

export function minimumMessageAgeMinutes(value: unknown): number {
  const minutes = value ?? 5;
  need(Number.isInteger(minutes) && Number(minutes) >= 0 && Number(minutes) <= 24 * 60,
    'secretary_delivery_config_invalid', 'Secretary minimum message age must be a whole number of minutes between zero and one day.');
  return Number(minutes);
}

export async function loadMinimumMessageAgeMinutes(path: string): Promise<number> {
  const config = await loadServiceConfiguration(path, 1024 * 1024,
    () => new IvyError('secretary_delivery_config_unprotected', 'Secretary service configuration must be a bounded protected regular file.'));
  return minimumMessageAgeMinutes(config?.['minimumMessageAgeMinutes']);
}
