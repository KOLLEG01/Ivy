import { hashJson } from '../../contracts/src/canonical.js';
/** One logical release ID for exact source bytes and the universal package recipe. */
export function candidateBuildId(componentId: string, version: string, sourceIdentity: string): string {
  return hashJson({ schemaVersion: 3, packageRecipe: 1, componentId, version, sourceIdentity });
}
