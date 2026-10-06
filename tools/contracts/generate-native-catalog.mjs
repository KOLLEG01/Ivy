import { readFileSync, writeFileSync, readdirSync } from 'node:fs';
import { gunzipSync } from 'node:zlib';
import { generateProjection } from './native-projection.mjs';
import { nativeVersions } from './checked-native-versions.mjs';
for (const version of nativeVersions) {
  const root = 'specs/native/codex-' + version;
  for (const file of readdirSync(root).filter(name => /^source(?:\.(?:linux|win32|darwin)-(?:x64|arm64))?\.json\.gz$/.test(name))) {
  const input = JSON.parse(gunzipSync(readFileSync(root + '/' + file), { maxOutputLength: 32 * 1024 * 1024 }));
  const projection = generateProjection(input), content = JSON.stringify(projection) + '\n', target = root + '/' + file.replace(/^source/, 'catalog').replace(/\.gz$/, '');
  if (process.argv.includes('--check')) {
    if (readFileSync(target, 'utf8') !== content) throw new Error('Stale generated native catalog: ' + version);
  } else writeFileSync(target, content);
  if (!process.argv.includes('--check')) console.log(JSON.stringify({ version, clientRequests: projection.clientRequests.length, serverRequests: projection.serverRequests.length, serverNotifications: projection.serverNotifications.length,
    bytes: Buffer.byteLength(content), derivedLegacyTypes: projection.derivedLegacyTypes }));
  }
}
