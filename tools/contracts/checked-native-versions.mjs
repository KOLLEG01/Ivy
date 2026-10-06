import { readdirSync } from 'node:fs';
// Source-owned exports are the complete admitted protocol set; no latest-version lookup or fallback.
export const nativeVersions = readdirSync('specs/native', { withFileTypes: true })
  .filter(entry => entry.isDirectory() && /^codex-\d+\.\d+\.\d+$/.test(entry.name)).map(entry => entry.name.slice(6)).sort();
if (!nativeVersions.length) throw new Error('No checked native protocol catalogs.');
