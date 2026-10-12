import { operationSchema } from '../../../packages/contracts/src/core-validation.js';
import { requireThat } from '../../../packages/contracts/src/errors.js';
import type { Operation } from '../../../packages/contracts/src/generated.js';
import type { DataContract } from '../../../packages/contracts/src/types.js';
import type { AuthenticatedContext } from './store.js';
import { Objects } from './objects.js';

const contract: DataContract = {
  key: 'ivy/openai-configuration', version: '1.0.0', owner: { kind: 'hive' }, mediaType: 'application/json',
  retention: { objects: { mode: 'retain' }, revisions: { mode: 'bounded', maximumCount: 100 } },
  specMarkdown: 'Hive-wide OpenAI connection identity. The Console saves it and AgentManager disables this hosted app while managing direct MCP.',
  jsonSchema: (operationSchema['$defs'] as Record<string, DataContract['jsonSchema']>)['OpenaiConfiguration']!,
};
const path = '/ivy-openai-configuration';

export class OpenaiConfiguration {
  constructor(readonly objects: Objects) { objects.register(contract, contract.owner); }

  get(): Operation.OpenaiConfigurationSnapshot {
    const row = this.objects.store.get('SELECT id FROM objects WHERE path=?', path);
    if (!row) return { revision: 0, configuration: { schemaVersion: 1, appId: null } };
    const read = this.objects.read({ objectId: String(row['id']) });
    requireThat(read.object.contractKey === contract.key && read.content.encoding === 'json' && !read.object.effectivelyArchived,
      'target_conflict', 'The OpenAI configuration path contains a different or archived object.');
    return { revision: read.revision.revision, configuration: read.content.value as Operation.OpenaiConfiguration };
  }

  save(context: AuthenticatedContext, params: Operation.OpenaiSaveConfigurationParams): Operation.OpenaiConfigurationSnapshot {
    const saved = this.objects.writeHivePrepared(context, 'openai.saveConfiguration', { ...params }, () => {
      const current = this.get();
      requireThat(current.revision === params.expectedRevision, 'revision_conflict', 'OpenAI configuration changed. Reload before saving.');
      return {
        mutationId: params.mutationId, contractVersion: contract.version, references: {},
        content: { encoding: 'json', value: { ...params.configuration } },
        ...(current.revision ? { objectId: this.objects.stat({ path }).id, expectedRevision: current.revision }
          : { create: { contractKey: contract.key, parentId: null, ownerObjectId: null, name: path.slice(1) } }),
      };
    });
    return { revision: saved.revision.revision, configuration: params.configuration };
  }
}
