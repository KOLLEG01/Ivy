import type { DatabaseSync } from 'node:sqlite';
import type { Wire } from '../../../packages/contracts/src/generated.js';
import { requireThat } from '../../../packages/contracts/src/errors.js';
import { validateShared } from '../../../packages/contracts/src/core-validation.js';

export type ContractRead = { key: string; readScope?: Wire.ContractReadScope };

/** One read-domain interpretation for registry, uis, live deployment and stopped inspection. */
export function contractVersions(db: DatabaseSync, required: ContractRead): string[] {
  const scope = required.readScope ?? { roots: null, history: 'all', includeArchived: true, references: [] };
  validateShared('ContractReadScope', scope);
  for (const root of scope.roots ?? []) requireThat(db.prepare('SELECT 1 FROM objects WHERE id=?').get(root), 'not_found', 'A contract read root does not exist.');
  for (const pin of scope.references) requireThat(db.prepare('SELECT 1 FROM revisions WHERE object_id=? AND revision=? AND contract_key=?').get(pin.objectId, pin.revision, required.key), 'not_found', 'A required contract evidence revision does not exist.');
  // The complete retained family is already covered by the contract/version index.
  const versions = (scope.roots === null && scope.history === 'all' && scope.includeArchived
    ? db.prepare('SELECT DISTINCT contract_version FROM revisions WHERE contract_key=? ORDER BY contract_version LIMIT 65').all(required.key)
    : db.prepare(`WITH RECURSIVE selected(id) AS (
      SELECT value FROM json_each(?) UNION SELECT o.id FROM objects o JOIN selected s ON o.parent_id=s.id
    ) SELECT DISTINCT r.contract_version FROM revisions r JOIN objects o ON o.id=r.object_id
    WHERE r.contract_key=? AND (
      ((? OR o.id IN (SELECT id FROM selected)) AND (? OR o.effective_archive=0) AND (? OR r.revision=o.current_revision))
      OR EXISTS (SELECT 1 FROM json_each(?) p WHERE json_extract(p.value,'$.objectId')=r.object_id AND json_extract(p.value,'$.revision')=r.revision)
    ) ORDER BY r.contract_version LIMIT 65`).all(JSON.stringify(scope.roots ?? []), required.key, scope.roots === null ? 1 : 0,
      scope.includeArchived ? 1 : 0, scope.history === 'all' ? 1 : 0, JSON.stringify(scope.references))).map(row => String(row['contract_version']));
  requireThat(versions.length <= 64, 'limit_exceeded', 'A requested read domain has too many contract versions.');
  return versions;
}
