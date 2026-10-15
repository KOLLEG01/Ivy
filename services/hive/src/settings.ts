import { operationSchema } from '../../../packages/contracts/src/core-validation.js';
import { requireThat } from '../../../packages/contracts/src/errors.js';
import type { Operation } from '../../../packages/contracts/src/generated.js';
import type { DataContract } from '../../../packages/contracts/src/types.js';
import type { AuthenticatedContext } from './store.js';
import { Objects } from './objects.js';

const contract: DataContract = {
  key: 'ivy/settings', version: '1.0.0', owner: { kind: 'hive' }, mediaType: 'application/json',
  retention: { objects: { mode: 'retain' }, revisions: { mode: 'bounded', maximumCount: 100 } },
  specMarkdown: 'Hive-wide settings. Quiet hours govern automatic notifications and calls independently of optional services.',
  jsonSchema: (operationSchema['$defs'] as Record<string, DataContract['jsonSchema']>)['HiveSettings']!,
};
const path = '/ivy-settings';

export class HiveSettings {
  constructor(readonly objects: Objects) { objects.register(contract, contract.owner); }
  get(): Operation.HiveSettingsSnapshot {
    const row = this.objects.store.get('SELECT id FROM objects WHERE path=?', path);
    if (!row) return { revision: 0, settings: { schemaVersion: 1, quietHours: null } };
    const read = this.objects.read({ objectId: String(row['id']) });
    requireThat(read.object.contractKey === contract.key && read.content.encoding === 'json' && !read.object.effectivelyArchived,
      'target_conflict', 'The Hive settings path contains a different or archived object.');
    return { revision: read.revision.revision, settings: read.content.value as Operation.HiveSettings };
  }
  save(context: AuthenticatedContext, params: Operation.SettingsSaveParams): Operation.HiveSettingsSnapshot {
    const saved = this.objects.writeHivePrepared(context, 'settings.save', { ...params }, () => {
      const quiet = params.settings.quietHours;
      if (quiet) {
        requireThat(quiet.start !== quiet.end, 'invalid_arguments', 'Quiet hours need distinct start and end times.');
        try { new Intl.DateTimeFormat('en', { timeZone: quiet.timeZone }).format(); }
        catch { requireThat(false, 'invalid_arguments', 'Quiet hours need a supported IANA time zone.'); }
      }
      const current = this.get();
      requireThat(current.revision === params.expectedRevision, 'revision_conflict', 'Hive settings changed. Reload before saving.');
      return { mutationId: params.mutationId, contractVersion: contract.version, references: {},
        content: { encoding: 'json', value: { ...params.settings } },
        ...(current.revision ? { objectId: this.objects.stat({ path }).id, expectedRevision: current.revision }
          : { create: { contractKey: contract.key, parentId: null, ownerObjectId: null, name: path.slice(1) } }) };
    });
    return { revision: saved.revision.revision, settings: params.settings };
  }
}
