import { isAbsolute } from 'node:path';
import { IvyError } from '../../../packages/sdk/src/node.js';
import { need } from './schema.js';
import type { Settings } from './schema.js';
import { microsoftProfile, outlookArguments } from './outlook-projection.js';
import { validateMicrosoft } from './microsoft-schema.js';
import type { MicrosoftConfiguration } from './microsoft-schema.js';
import { loadServiceConfiguration } from './service-config.js';

export function microsoftConfiguration(value: unknown, settings: Settings): MicrosoftConfiguration {
  validateMicrosoft('MicrosoftConfiguration', value); const config = value as MicrosoftConfiguration;
  need(new Set(config.collectors.map(item => item.sourceId)).size === config.collectors.length, 'microsoft_source_config_invalid', 'Microsoft source IDs must be unique.');
  for (const collector of config.collectors) {
    const source = settings.sources.find(item => item.sourceId === collector.sourceId); microsoftProfile(collector.profile);
    need(source?.kind === 'email' && source.accountId === collector.profile.id && source.producerPrincipalIds.includes(settings.identity.principalId) && isAbsolute(collector.threadCwd),
      'microsoft_source_config_invalid', 'An Outlook collector requires this admitted producer, its exact account and an absolute thread directory.');
    const later = new Date(Date.parse(collector.since) + 1).toISOString(); outlookArguments({ since: collector.since, until: later, skip: 0, pageSize: collector.pageSize });
  }
  return structuredClone(config);
}
export async function loadMicrosoftConfiguration(path: string, settings: Settings): Promise<MicrosoftConfiguration> {
  const config=await loadServiceConfiguration(path,1024*1024,()=>new IvyError('microsoft_config_unprotected','Secretary service configuration must be a bounded protected regular file.'));
  return config?.['microsoft']===undefined ? {schemaVersion:1,collectors:[]} : microsoftConfiguration(config['microsoft'],settings);
}
