import { test } from 'node:test';
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';

test('Windows lab resource guards and durable intent replay', { skip: process.platform !== 'win32' }, () => {
  const result = spawnSync('powershell.exe', ['-NoProfile', '-NonInteractive', '-File',
    fileURLToPath(new URL('../fixtures/ivy-rust-lab.test.ps1', import.meta.url))],
  { encoding: 'utf8', timeout: 20_000, windowsHide: true });
  assert.ifError(result.error);
  assert.equal(result.status, 0, result.stderr || result.stdout);
});

test('Windows lab provision, baseline drift and rollback orchestration', { skip: process.platform !== 'win32' }, () => {
  const result = spawnSync('powershell.exe', ['-NoProfile', '-NonInteractive', '-File',
    fileURLToPath(new URL('../fixtures/ivy-rust-lab-orchestration.test.ps1', import.meta.url))],
  { encoding: 'utf8', timeout: 30_000, windowsHide: true });
  assert.ifError(result.error);
  assert.equal(result.status, 0, result.stderr || result.stdout);
});
