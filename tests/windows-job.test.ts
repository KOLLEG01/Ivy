import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, readFile, writeFile, rm } from 'node:fs/promises';
import { execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { tmpdir } from 'node:os';
import { join, resolve, relative } from 'node:path';

const exec = promisify(execFile);
test('Windows launcher death immediately after child creation reaps the original suspended child', { skip: process.platform !== 'win32', timeout: 90_000 }, async t => {
  const root = await mkdtemp(join(tmpdir(), 'ivy-job-creation-'));
  t.after(async () => { assert.ok(relative(tmpdir(), root).startsWith('ivy-job-creation-')); await rm(root, { recursive: true, force: true }); });
  const compiler = join(process.env['SystemRoot'] ?? 'C:/Windows', 'Microsoft.NET/Framework64/v4.0.30319/csc.exe');
  const source = await readFile('packages/host-runtime/native/JobLauncher.cs', 'utf8');
  const creation = /^\s*Check\(CreateProcess\(.*out child\)\);$/gm;
  assert.equal([...source.matchAll(creation)].length, 1, 'Fault injection needs one real successful child-creation boundary.');
  const probe = join(root, 'probe.exe');
  await exec(compiler, ['/nologo', '/target:exe', '/platform:x64', '/out:' + probe, resolve('tests/fixtures/windows-job-crash.cs')], { windowsHide: true, timeout: 30_000 });
  for (const target of ['exe', 'winexe']) await t.test(target, async () => {
    const marker = join(root, target + '-created.txt'), launcher = join(root, target + '-launcher.exe'), instrumented = join(root, target + '-launcher.cs');
    // Pause immediately after the actual OS creation returns, before any later launcher code.
    // This would leave an unassigned child alive in the previous two-call implementation.
    await writeFile(instrumented, source.replace(creation, line => line + '\n            System.IO.File.WriteAllText(@"' + marker.replaceAll('"', '""') + '", child.ProcessId.ToString()); System.Threading.Thread.Sleep(30000);'));
    await exec(compiler, ['/nologo', '/target:' + target, '/platform:x64', '/out:' + launcher, instrumented], { windowsHide: true, timeout: 30_000 });
    const result = await exec(probe, [launcher, process.execPath, marker], { windowsHide: true, timeout: 30_000, maxBuffer: 16384 });
    const evidence = JSON.parse(result.stdout) as { launcherExited: boolean; childExited: boolean; childPid: number };
    assert.equal(evidence.launcherExited, true); assert.equal(evidence.childExited, true);
    assert.ok(Number.isInteger(evidence.childPid) && evidence.childPid > 0);
  });
});
