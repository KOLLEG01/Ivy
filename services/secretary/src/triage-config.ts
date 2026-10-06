import { isAbsolute } from 'node:path';
import { IvyError } from '../../../packages/sdk/src/node.js';
import type { Settings } from './schema.js';
import { triageCheck } from './triage-store.js';
import { validateTriage } from './triage-schema.js';
import type { TriageSettings } from './triage-schema.js';
import { loadServiceConfiguration } from './service-config.js';
export function triageConfiguration(value: unknown, _settings: Settings): TriageSettings {
  validateTriage('TriageSettings', value); const config = value as TriageSettings;
  triageCheck(isAbsolute(config.threadCwd), 'secretary_triage_config_invalid');
  return structuredClone(config);
}
export async function loadTriageConfiguration(path: string, settings: Settings): Promise<TriageSettings | null> {
  const config=await loadServiceConfiguration(path,1024*1024,()=>new IvyError('secretary_triage_config_unprotected','Secretary service configuration must be a bounded protected regular file.'));
  return config?.['triage']===undefined ? null : triageConfiguration(config['triage'],settings);
}
