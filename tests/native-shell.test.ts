import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, readFile, writeFile, rm } from 'node:fs/promises';
import { join, resolve, relative } from 'node:path';
import { tmpdir } from 'node:os';
import { execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { digest } from '../packages/contracts/src/canonical.js';
import { IvyError } from '../packages/contracts/src/errors.js';
import { verifyWindowsShell, windowsShellEnvironment } from '../services/agent-manager/src/windows-shell.js';

test('Windows shell identity fixture refuses changed bytes and package identity before native launch', { timeout: 20_000, skip: process.platform !== 'win32' }, async t => {
  const root = await mkdtemp(join(tmpdir(), 'ivy-native-shell-test-')), executable = join(root, 'pwsh.exe'), source = join(root, 'Shell.cs');
  t.after(async () => { assert.ok(relative(tmpdir(), root).startsWith('ivy-native-shell-test-')); await rm(root, { recursive: true, force: true }); });
  // This executable is an explicit probe-response fixture, not an actual PowerShell distribution.
  await writeFile(source, `using System; using System.IO; using System.Diagnostics; public static class Shell { public static void Main() {
    string path=Process.GetCurrentProcess().MainModule.FileName, dir=Path.GetDirectoryName(path); File.WriteAllText(Path.Combine(dir,"invoked"),"yes");
    int status=File.Exists(Path.Combine(dir,"packaged"))?122:15700;
    Console.WriteLine("{\\"packageStatus\\":"+status+",\\"version\\":\\"7.6.5\\",\\"executable\\":\\""+path.Replace("\\\\","\\\\\\\\")+"\\"}");
  } }`);
  await promisify(execFile)(join(process.env['SystemRoot']!, 'Microsoft.NET/Framework64/v4.0.30319/csc.exe'),
    ['/nologo', '/target:exe', '/platform:x64', '/out:' + executable, source], { windowsHide: true, timeout: 10_000 });
  const launcher = resolve('dist/native/ivy-job.exe'), settings = { executable, executableHash: digest(await readFile(executable)) };
  await assert.rejects(verifyWindowsShell(undefined, root, launcher), (error: unknown) => error instanceof IvyError && error.code === 'native_shell_required');
  await assert.rejects(verifyWindowsShell({ ...settings, executableHash: digest('wrong') }, root, launcher), (error: unknown) => error instanceof IvyError && error.code === 'native_shell_mismatch');
  await assert.rejects(readFile(join(root, 'invoked')), (error: NodeJS.ErrnoException) => error.code === 'ENOENT');
  const verified = await verifyWindowsShell(settings, root, launcher);
  assert.equal(verified.executable, executable); assert.equal(verified.packageIdentity, 'unpackaged'); assert.equal(verified.version, '7.6.5');
  const originalPath = Object.entries(process.env).find(([key]) => key.toLowerCase() === 'path');
  assert.ok(windowsShellEnvironment(verified)['PATH']!.startsWith(root + ';'));
  assert.equal(process.env[originalPath![0]], originalPath![1]);
  await writeFile(join(root, 'packaged'), 'fixture');
  await assert.rejects(verifyWindowsShell(settings, root, launcher), (error: unknown) => error instanceof IvyError && error.code === 'native_shell_packaged');
});
