import { spawnSync } from 'node:child_process';
import { mkdtempSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';

function run(command, args, options = {}) {
  const result = spawnSync(command, args, { encoding: 'utf8', timeout: 120_000, windowsHide: true, ...options });
  if (result.error || result.status !== 0) throw result.error ?? new Error(result.stderr || `${command} failed.`);
  return result.stdout?.trim() ?? '';
}

const status = run('git', ['status', '--porcelain', '--untracked-files=no']);
if (status) throw new Error('Commit the Ivy source tree before publishing an SDK ref.');
const root = JSON.parse(readFileSync('package.json', 'utf8'));
const sourceCommit = run('git', ['rev-parse', 'HEAD']);
const remote = run('git', ['remote', 'get-url', 'origin']);
const tag = `sdk-v${root.version}`;
if (run('git', ['ls-remote', '--tags', 'origin', `refs/tags/${tag}`])) throw new Error(`Remote tag ${tag} already exists.`);

run(process.execPath, ['tools/package/package-sdk.mjs'], { stdio: 'inherit' });
const receipt = JSON.parse(readFileSync(resolve('.local/packages/sdk-package.json'), 'utf8'));
if (receipt.version !== root.version || receipt.sourceCommit !== sourceCommit) throw new Error('SDK package provenance does not match the committed source.');

const temporary = mkdtempSync(join(tmpdir(), 'ivy-sdk-ref-'));
try {
  run('tar', ['-xf', receipt.path, '-C', temporary]);
  const packageRoot = join(temporary, 'package');
  run('git', ['init', '--quiet'], { cwd: packageRoot });
  run('git', ['config', 'user.name', 'Ivy SDK Release'], { cwd: packageRoot });
  run('git', ['config', 'user.email', 'ivy-sdk@users.noreply.github.com'], { cwd: packageRoot });
  run('git', ['add', '--all'], { cwd: packageRoot });
  run('git', ['commit', '--quiet', '-m', `Release @ivy/sdk ${root.version} from ${sourceCommit}`], { cwd: packageRoot });
  const packageCommit = run('git', ['rev-parse', 'HEAD'], { cwd: packageRoot });
  run('git', ['tag', '-a', tag, '-m', `@ivy/sdk ${root.version}`], { cwd: packageRoot });
  run('git', ['remote', 'add', 'origin', remote], { cwd: packageRoot });
  run('git', ['push', 'origin', `refs/tags/${tag}`], { cwd: packageRoot, stdio: 'inherit', timeout: 180_000 });
  console.log(JSON.stringify({ tag, packageCommit, sourceCommit, dependency: `git+${remote}#${packageCommit}` }));
} finally {
  rmSync(temporary, { recursive: true, force: true });
}
