import { spawnSync } from 'node:child_process';
import { existsSync, readFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
const bin = dirname(process.execPath);
const candidates = [resolve(bin, 'node_modules/npm/bin/npm-cli.js'), resolve(bin, '../lib/node_modules/npm/bin/npm-cli.js'), resolve(bin, '../node_modules/npm/bin/npm-cli.js')];
const cli = candidates.find(path => existsSync(path));
if (!cli) throw new Error('The isolated Node toolchain does not contain npm.');
const expected = JSON.parse(readFileSync('package.json', 'utf8')).packageManager;
const actual = 'npm@' + JSON.parse(readFileSync(resolve(cli, '../../package.json'), 'utf8')).version;
if (actual !== expected) throw new Error(`Pinned package manager required: ${expected}; observed ${actual}`);
// Host preparation restores an immutable, lock/runtime-keyed dependency cache before
// invoking the plan. Keep the plan contract stable while avoiding a second npm install.
if (process.argv[2] === 'ci' && process.env['IVY_DEPENDENCY_CACHE_HIT'] === '1') {
  process.exitCode = 0;
} else {
const result = spawnSync(process.execPath, [cli, ...process.argv.slice(2)], { stdio: 'inherit', windowsHide: true, timeout: 600_000 });
if (result.error) throw result.error;
process.exitCode = result.status ?? 1;
}
