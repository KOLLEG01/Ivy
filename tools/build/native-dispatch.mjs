// Runtime asset copied into the native package by build-native-dispatchers.mjs.
import { realpathSync, statSync } from 'node:fs';
import { resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { spawn } from 'node:child_process';

const platforms = new Set(['win32-x64', 'win32-arm64', 'linux-x64', 'linux-arm64']);
function failure(code) {
  return Object.assign(new Error(code), { code });
}
export function nativeExecutable(root, name, platform = process.platform, arch = process.arch) {
  if (!platforms.has(`${platform}-${arch}`)) throw failure('unsupported_platform');
  if (!/^ivy-[a-z][a-z0-9-]*$/.test(name)) throw failure('invalid_arguments');
  const path = resolve(root, `${platform}-${arch}`, name + (platform === 'win32' ? '.exe' : ''));
  try {
    if (realpathSync(path) !== path || !statSync(path).isFile()) throw failure('artifact_missing');
  } catch {
    throw failure('artifact_missing');
  }
  return path;
}

export async function dispatch(name, shutdownTimeoutMs) {
  const executable = nativeExecutable(fileURLToPath(new URL('.', import.meta.url)), name);
  const child = spawn(executable, process.argv.slice(2), {
    stdio: 'inherit', windowsHide: true,
    // Keep the host's Job/process-group membership and its launch identity.
    env: { ...process.env, IVY_NODE_EXECUTABLE: process.execPath },
  });
  let timer;
  const handlers = new Map(['SIGINT', 'SIGTERM', 'SIGHUP'].map(signal => [signal, () => {
    if (timer) return;
    child.kill(signal);
    timer = setTimeout(() => child.kill('SIGKILL'), shutdownTimeoutMs);
    timer.unref();
  }]));
  for (const [signal, handler] of handlers) process.on(signal, handler);
  let exit;
  try {
    exit = await new Promise((resolve, reject) => {
      child.once('error', reject);
      child.once('exit', (code, signal) => resolve({ code, signal }));
    });
  } finally {
    clearTimeout(timer);
    for (const [signal, handler] of handlers) process.removeListener(signal, handler);
  }
  if (exit.signal) process.kill(process.pid, exit.signal);
  else process.exitCode = exit.code ?? 1;
}

export function dispatchFailure(error, health) {
  const code = ['unsupported_platform', 'artifact_missing', 'invalid_arguments'].includes(error?.code) ? error.code : 'internal_error';
  if (health) process.stdout.write(JSON.stringify({ ok: false, code }) + '\n');
  else process.stderr.write(`Native dispatch failed: ${code}.\n`);
  process.exitCode = 1;
}
