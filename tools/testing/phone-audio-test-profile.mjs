import assert from 'node:assert/strict';
import { open } from 'node:fs/promises';
import { isAbsolute } from 'node:path';
import { jsonFile } from '../../dist/packages/host-runtime/src/config.js';
import { phoneSettings } from '../../dist/services/phone-bridge/src/runtime/settings.js';
import { audioTestProfile } from '../../dist/services/phone-bridge/src/runtime/audio-test-profile.js';

assert.equal(process.argv.length, 6, 'Usage: node tools/testing/phone-audio-test-profile.mjs <absolute-service-config.json> <fresh-output.json> <G722Only|EvsOnly|EvsPreferred|G722Preferred> <Optimized|Conservative>');
const [input, output, codecProfile, latencyPreset] = process.argv.slice(2);
assert.ok(isAbsolute(input) && isAbsolute(output), 'Absolute input and fresh output paths required.');
const config = await jsonFile(input, 1048576);
phoneSettings(config.settings);
config.settings = audioTestProfile(config.settings, codecProfile, latencyPreset);
phoneSettings(config.settings);
const file = await open(output, 'wx', 0o600);
try { await file.writeFile(JSON.stringify(config, null, 2) + '\n'); await file.sync(); }
finally { await file.close(); }
process.stdout.write(JSON.stringify({ state: 'prepared', codecProfile, latencyPreset, restartRequired: true }) + '\n');
