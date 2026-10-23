import assert from 'node:assert/strict';
import { open } from 'node:fs/promises';
import { isAbsolute, join } from 'node:path';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { jsonFile } from '../../dist/packages/host-runtime/src/config.js';
import { audioTestProfile } from '../../dist/tools/testing/phone-audio-profile.js';

assert.equal(process.argv.length, 6, 'Usage: node tools/testing/phone-audio-test-profile.mjs <absolute-service-config.json> <fresh-output.json> <G722Only|EvsOnly|EvsPreferred|G722Preferred> <Optimized|Conservative>');
const [input, output, codecProfile, latencyPreset] = process.argv.slice(2);
assert.ok(isAbsolute(input) && isAbsolute(output), 'Absolute input and fresh output paths required.');
const config = await jsonFile(input, 1048576);
const artifactRoot = config.artifactRoot ?? fileURLToPath(new URL('../../', import.meta.url));
assert.ok(typeof artifactRoot === 'string' && isAbsolute(artifactRoot), 'An absolute service artifact root is required.');
function validateSettings(settings) {
  const executable = join(artifactRoot, `dist/native/${process.platform}-${process.arch}/ivy-phone-bridge${process.platform === 'win32' ? '.exe' : ''}`);
  const result = spawnSync(executable, ['--validate-settings'], {
    input: JSON.stringify(settings), encoding: 'utf8', windowsHide: true, timeout: 15000,
  });
  if (result.error) throw result.error;
  assert.equal(result.status, 0, result.stderr || 'Native Phone settings validation failed.');
}
validateSettings(config.settings);
config.settings = audioTestProfile(config.settings, codecProfile, latencyPreset);
validateSettings(config.settings);
const file = await open(output, 'wx', 0o600);
try { await file.writeFile(JSON.stringify(config, null, 2) + '\n'); await file.sync(); }
finally { await file.close(); }
process.stdout.write(JSON.stringify({ state: 'prepared', codecProfile, latencyPreset, restartRequired: true }) + '\n');
