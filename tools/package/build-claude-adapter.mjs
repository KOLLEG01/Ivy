import { createHash } from 'node:crypto';
import { copyFileSync, existsSync, mkdirSync, readFileSync, readdirSync, statSync } from 'node:fs';
import { isAbsolute, resolve, join } from 'node:path';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';

const root = resolve(import.meta.dirname, '../..');
const source = join(root, 'services/agent-manager/claude');
const lock = JSON.parse(readFileSync(join(source, 'adapter.lock.json'), 'utf8'));
export function claudeAdapterBundleHash(directory) {
  const files = ['package-lock.json', ...readdirSync(join(directory, 'bridge'))
    .filter(name => name.endsWith('.mjs')).map(name => 'bridge/' + name)].sort();
  const hash = createHash('sha256');
  for (const name of files) {
    const file = join(directory, name);
    if (!statSync(file).isFile()) throw new Error('SDK bundle member is not a regular file: ' + name);
    hash.update(name).update('\0').update(createHash('sha256').update(readFileSync(file)).digest('hex')).update('\0');
  }
  return 'sha256:' + hash.digest('hex');
}
export function buildClaudeSdkBridge(target) {
  if (!isAbsolute(target) || existsSync(target)) throw new Error('Use a new absolute output directory');
  const packageLock = JSON.parse(readFileSync(join(source, 'package-lock.json'), 'utf8'));
  if (packageLock.packages['node_modules/@anthropic-ai/claude-agent-sdk']?.version !== lock.agentSdkVersion)
    throw new Error('Agent SDK lock differs');
  if (claudeAdapterBundleHash(source) !== lock.bundleHash) throw new Error('Pinned SDK bridge differs');
  mkdirSync(join(target, 'bridge'), { recursive: true });
  for (const name of ['package.json', 'package-lock.json', 'bridge/main.mjs', 'bridge/session.mjs'])
    copyFileSync(join(source, name), join(target, name));
  const installed = spawnSync('npm', ['ci', '--ignore-scripts'], { cwd: target, stdio: 'inherit',
    windowsHide: true, shell: process.platform === 'win32' });
  if (installed.status !== 0) throw new Error('Pinned vendor SDK installation failed');
  if (claudeAdapterBundleHash(target) !== lock.bundleHash) throw new Error('Installed SDK bridge differs');
  return target;
}
if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const index = process.argv.indexOf('--out');
  if (index < 0 || !process.argv[index + 1]) throw new Error('Use --out with a new absolute directory');
  buildClaudeSdkBridge(process.argv[index + 1]);
}
