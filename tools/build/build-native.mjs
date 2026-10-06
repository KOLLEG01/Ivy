import { spawnSync } from 'node:child_process';
import { mkdirSync, existsSync, readFileSync, writeFileSync, openSync, closeSync, unlinkSync } from 'node:fs';
import { resolve, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { fingerprint } from './build-cache.mjs';
const componentOutputs = {
  'host-executor': ['ivy-job.exe', 'ivy-host-job.exe'],
  'agent-manager': ['ivy-job.exe'],
  'phone-bridge': ['ivy-job.exe', 'ivy-phone-input.dll'],
};
export function nativeOutputNames(component) {
  return component ? (componentOutputs[component] ?? ['ivy-job.exe']) : ['ivy-job.exe', 'ivy-host-job.exe', 'ivy-phone-input.dll'];
}

export function buildNative({ component } = {}) {
  if (process.platform !== 'win32') return;
  const compiler = join(process.env.SystemRoot ?? 'C:\\Windows', 'Microsoft.NET', 'Framework64', 'v4.0.30319', 'csc.exe');
  mkdirSync('dist/native', { recursive: true });
  const names = nativeOutputNames(component);
  const outputs = names.map(name => 'dist/native/' + name);
  const inputs = ['tools/build/build-native.mjs', ...names.includes('ivy-job.exe') || names.includes('ivy-host-job.exe') ? ['packages/host-runtime/native/JobLauncher.cs'] : [], ...names.includes('ivy-phone-input.dll') ? ['services/phone-bridge/native/input/VoiceHotkey.cs', 'services/phone-bridge/native/input/DesktopLaunch.cs', 'services/phone-bridge/native/input/CaptureObservation.cs', 'services/phone-bridge/native/input/AudioProcessOwnership.cs'] : []];
  const stamp = 'dist/.build-cache/native' + (component ? '-' + component : '') + '.json';
  const input = fingerprint(inputs, { compiler, platform: process.platform, arch: process.arch });
  let previous;
  try { previous = JSON.parse(readFileSync(stamp, 'utf8')); } catch {}
  if (!process.argv.includes('--force') && outputs.every(existsSync) && previous?.input === input && previous.output === fingerprint(outputs)) return;
  const lockPath = 'dist/.build-cache/native.lock'; mkdirSync('dist/.build-cache', { recursive: true });
  let lock;
  for (let attempt = 0; attempt < 300 && lock === undefined; attempt++) {
    try { lock = openSync(lockPath, 'wx'); } catch { Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, 100); }
  }
  if (lock === undefined) throw new Error('Timed out waiting for the Native build lock.');
  {
    try {
      for (const [target, name] of [['exe', 'ivy-job.exe'], ['winexe', 'ivy-host-job.exe']]) {
        if (!names.includes(name)) continue;
        const result = spawnSync(compiler, ['/nologo', '/optimize+', '/target:' + target, '/platform:x64', '/out:' + resolve('dist/native/' + name), resolve('packages/host-runtime/native/JobLauncher.cs')], { windowsHide: true, encoding: 'utf8', timeout: 60_000 });
        if (result.error || result.status !== 0) throw result.error ?? new Error(result.stdout + result.stderr);
      }
      if (names.includes('ivy-phone-input.dll')) {
        const voice = spawnSync(compiler, ['/nologo', '/optimize+', '/target:library', '/platform:x64', '/out:' + resolve('dist/native/ivy-phone-input.dll'), resolve('services/phone-bridge/native/input/VoiceHotkey.cs'), resolve('services/phone-bridge/native/input/DesktopLaunch.cs'), resolve('services/phone-bridge/native/input/CaptureObservation.cs'), resolve('services/phone-bridge/native/input/AudioProcessOwnership.cs')], { windowsHide: true, encoding: 'utf8', timeout: 60_000 });
        if (voice.error || voice.status !== 0) throw voice.error ?? new Error(voice.stdout + voice.stderr);
      }
    } finally { closeSync(lock); try { unlinkSync(lockPath); } catch {} }
  }
  writeFileSync(stamp, JSON.stringify({ input, output: fingerprint(outputs) }));
}
if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) buildNative();
