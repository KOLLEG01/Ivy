import { IvyError } from '../../../packages/sdk/src/node.js';
import { need } from './schema.js';
import type { Settings } from './schema.js';
import { validateMicrosoft } from './microsoft-schema.js';
import type { TeamsConfiguration } from './teams-schema.js';
import { loadServiceConfiguration } from './service-config.js';

export function teamsConfiguration(value: unknown, _settings: Settings): TeamsConfiguration {
  validateMicrosoft('TeamsConfiguration', value); const config = value as TeamsConfiguration;
  need(config.collectors.length === 0, 'teams_native_source_not_ready', 'Use an admitted source producer. Native Teams collection is disabled until complete pagination and media coverage are supported.');
  return structuredClone(config);
}
export async function loadTeamsConfiguration(path: string, settings: Settings): Promise<TeamsConfiguration> {
  const config=await loadServiceConfiguration(path,1024*1024,()=>new IvyError('teams_config_unprotected','Secretary service configuration must be a bounded protected regular file.'));
  return config?.['teams']===undefined ? {schemaVersion:1,collectors:[]} : teamsConfiguration(config['teams'],settings);
}
