import { parseArgs } from 'node:util';
import { spawnSync } from 'node:child_process';
import { randomUUID } from 'node:crypto';
import { existsSync, lstatSync, mkdirSync, readFileSync, realpathSync, writeFileSync } from 'node:fs';
import { dirname, isAbsolute, relative, resolve } from 'node:path';
import { buildIdentity } from '../../../packages/host-runtime/src/build-identity.mjs';

// Capture tracked edits and new source files without changing the user's index/HEAD.
// Output/branding and ignored runtime data are deliberately not acceptance source.
const { values } = parseArgs({ options: { source: { type: 'string' }, destination: { type: 'string' }, report: { type: 'string' } } });
if (!values.source || !values.destination || !values.report) throw new Error('Required: --source PATH --destination FRESH_PATH --report PATH');
const source = realpathSync(resolve(values.source)), destination = resolve(values.destination), reportPath = resolve(values.report);
const contained = (root, path) => { const p = relative(root, path); return !isAbsolute(p) && p !== '..' && !p.startsWith('..\\') && !p.startsWith('../'); };
const localRoot = resolve(source, '.local');
if ((contained(source, destination) && !contained(localRoot, destination)) || existsSync(destination) || existsSync(reportPath) || contained(destination, reportPath))
  throw new Error('Use a fresh destination outside source or below its ignored .local directory, and a fresh report outside destination.');
function git(cwd, args) {
  const result = spawnSync('git', args, { cwd, encoding: 'utf8', windowsHide: true, timeout: 60000, maxBuffer: 8 * 1024 * 1024 });
  if (result.error || result.status !== 0) throw result.error ?? new Error(result.stderr);
  return result.stdout.trimEnd();
}
const baseCommit = git(source, ['rev-parse', 'HEAD']);
if (contained(source, destination)) {
  const ignored = spawnSync('git', ['check-ignore', '--quiet', '--', relative(source, destination)], { cwd: source, windowsHide: true, timeout: 10000 });
  if (ignored.error || ignored.status !== 0) throw new Error('An in-repository acceptance snapshot must be ignored by Git.');
}
const excluded = ['output/', 'docs/logo/'];
const paths = [...new Set(git(source, ['ls-files', '--cached', '--others', '--exclude-standard', '-z']).split('\0').filter(Boolean))]
  .filter(path => !excluded.some(prefix => path.startsWith(prefix)) && existsSync(resolve(source, path))).sort();
if (paths.length > 20000) throw new Error('Too many source files.');
mkdirSync(destination, { recursive: true });
let totalBytes = 0;
for (const path of paths) {
  const input = resolve(source, path), output = resolve(destination, path);
  if (!contained(source, input) || !contained(destination, output) || /(^|\/)(node_modules|dist|runtime|\.local|config\.json|\.env(?:\..*)?)($|\/)/.test(path)) throw new Error('Unsafe source path: ' + path);
  if (!lstatSync(input).isFile() || !contained(source, realpathSync(input))) throw new Error('Source must be an ordinary contained file: ' + path);
  const bytes = readFileSync(input); totalBytes += bytes.length;
  if (bytes.length > 64 * 1024 * 1024 || totalBytes > 1024 * 1024 * 1024) throw new Error('Source byte limit exceeded.');
  mkdirSync(dirname(output), { recursive: true }); writeFileSync(output, bytes, { flag: 'wx' });
}
const identity = buildIdentity(destination);
const report = { schemaVersion: 1, capturedAt: new Date().toISOString(), source, destination, baseCommit,
  snapshotId: randomUUID(), identity, excluded, fileCount: paths.length, totalBytes };
mkdirSync(dirname(reportPath), { recursive: true }); writeFileSync(reportPath, JSON.stringify(report, null, 2) + '\n', { flag: 'wx' });
console.log(JSON.stringify({ report: reportPath, destination, baseCommit, snapshotId: report.snapshotId, identity, fileCount: paths.length, totalBytes }));
