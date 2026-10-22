import { hashJson } from '../../contracts/src/canonical.js';
/** One logical release ID for source bytes and the selected native artifact, when present. */
export function candidateBuildId(componentId: string, version: string, sourceIdentity: string, nativePlatform?: string): string {
  return hashJson({ schemaVersion: 3, packageRecipe: 1, componentId, version, sourceIdentity,
    ...(nativePlatform ? { nativePlatform } : {}) });
}
