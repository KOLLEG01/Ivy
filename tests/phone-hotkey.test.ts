import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve, relative } from 'node:path';
import { runCommand } from '../packages/host-runtime/src/process.js';

test('Windows Voice input and normal Desktop launch preserve unknown results without real input or activation',
  { skip: process.platform !== 'win32', timeout: 30_000 }, async t => {
    const root = await mkdtemp(join(tmpdir(), 'ivy-phone-input-'));
    t.after(async () => { assert.ok(relative(tmpdir(), root).startsWith('ivy-phone-input-')); await rm(root, { recursive: true, force: true }); });
    const compiler = join(process.env['SystemRoot'] ?? 'C:\\Windows', 'Microsoft.NET', 'Framework64', 'v4.0.30319', 'csc.exe');
    const output = join(root, 'voice-input-tests.exe');
    await runCommand({ executable: 'compiler', args: ['/nologo', '/target:exe', '/platform:x64', '/main:VoiceHotkeyTests', '/out:' + output,
      resolve('services/phone-bridge/native/input/VoiceHotkey.cs'), resolve('services/phone-bridge/native/input/DesktopLaunch.cs'), resolve('services/phone-bridge/native/input/CaptureObservation.cs'),
      resolve('services/phone-bridge/native/input/AudioProcessOwnership.cs'), resolve('tests/native/AudioProcessOwnershipTests.cs'),
      resolve('tests/native/VoiceHotkeyTests.cs'), resolve('tests/native/DesktopLaunchTests.cs'), resolve('tests/native/CaptureObservationTests.cs')], timeoutMs: 15_000 }, resolve('.'), { compiler });
    const result = await runCommand({ executable: 'checks', args: [], timeoutMs: 10_000 }, root, { checks: output },
      { jobLauncher: resolve('dist/native/ivy-job.exe') });
    assert.match(result.stdout, /^voice_hotkey_unit_passed:/);
    assert.match(result.stdout, /desktop_launch_unit_passed:/);
    assert.match(result.stdout, /capture_observation_unit_passed:/);
    assert.match(result.stdout, /audio_process_ownership_unit_passed:/);
  });
