import { spawnSync } from 'node:child_process';
import { resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { mkdtempSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { preparePhoneEvs, installPhoneEvs } from '../package/phone-evs-package.mjs';

// Dedicated component check: no carrier registration, device opening or Desktop action. Full product
// acceptance also needs the installed call/media flow. SIP peers bind only127.0.0.1 with synthetic PCM.
assert.equal(process.platform, 'win32', 'Phone runtime requires Windows x64.');
assert.equal(process.arch, 'x64');
const root = fileURLToPath(new URL('../../', import.meta.url));
const cwd = resolve(root, 'services/phone-bridge/native/phone-runtime'), dotnet = process.env.IVY_DOTNET ?? 'dotnet';
function run(args, timeout) {
  const result = spawnSync(dotnet, args, { cwd, encoding: 'utf8', windowsHide: true, timeout, maxBuffer: 2 * 1024 * 1024,
    env: { ...process.env, DOTNET_CLI_TELEMETRY_OPTOUT: '1', DOTNET_NOLOGO: '1' } });
  if (result.error) throw result.error;
  if (result.status !== 0) throw new Error(`Phone runtime check failed (${result.status}):\n${result.stdout}\n${result.stderr}`);
  return result.stdout;
}
assert.equal(run(['--version'], 15_000).trim(), '10.0.302');
const project = resolve(root, 'tests/native-phone/Ivy.PhoneRuntime.Tests.csproj');
run(['restore', project, '--locked-mode', '-p:NuGetAudit=false', '--configfile', resolve(cwd, 'NuGet.Config')], 180_000);
// A regression check must not consume native DLLs left by another build/cache.
const output = mkdtempSync(resolve(tmpdir(), 'ivy-phone-check-'));
process.env.IVY_TEST_TEMP = output;
try {
run(['build', project, '--no-restore', '-c', 'Release', '--output', output], 120_000);
const evs = await preparePhoneEvs(root);
await installPhoneEvs(root, output, evs);
// A misspelled focused mode must not silently run the general suite and satisfy marker checks.
const invalidMode = spawnSync(dotnet, [resolve(output, 'Ivy.PhoneRuntime.Tests.dll'), 'desktop-controls-only'],
  { cwd, encoding: 'utf8', windowsHide: true, timeout: 15_000, maxBuffer: 1024 * 1024 });
assert.equal(invalidMode.error, undefined); assert.equal(invalidMode.status, 1);
assert.equal(invalidMode.stdout, ''); assert.match(invalidMode.stderr, /Unknown or incomplete native test mode/);
const result = run([resolve(output, 'Ivy.PhoneRuntime.Tests.dll'), '--loopback-sip',
  '--production-executable', resolve(output, 'Ivy.PhoneRuntime.dll')], 90_000);
assert.match(result, /^phone_native_composition_passed:/m);
assert.match(result, /^phone_native_owner_passed:/m);
assert.match(result, /^phone_incoming_identity_passed:/m);
assert.match(result, /^phone_audio_unit_passed:/m);
assert.match(result, /^phone_call_audio_unit_passed:/m);
assert.match(result, /^phone_call_audio_loopback_passed:/m);
assert.match(result, /^phone_codec_unit_passed:/m);
assert.match(result, /^phone_evs_managed_passed:/m);
assert.match(result, /^phone_rtp_unit_passed:/m);
assert.match(result, /^phone_rtp_loopback_passed:/m);
assert.match(result, /^phone_rpc_unit_passed:/m);
assert.match(result, /^phone_desktop_commands_passed:/m);
assert.match(result, /^phone_call_owner_passed:/m);
assert.match(result, /^phone_capture_commands_passed:/m);
assert.match(result, /^phone_capture_correlation_passed:/m);
assert.match(result, /^phone_desktop_controls_passed:/m);
assert.match(result, /^phone_voice_controls_passed:/m);
assert.match(result, /^phone_voice_refresh_passed:/m);
assert.match(result, /^phone_voice_authority_passed:/m);
assert.match(result, /^phone_voice_lifetime_passed:/m);
assert.match(result, /^phone_pipe_loopback_passed:/m);
assert.match(result, /^phone_sip_loopback_passed:/m);
assert.match(result, /^phone_invite_exchange_passed:/m);
assert.match(result, /^phone_delayed_offer_passed:/m);
} finally {
  // On Windows the process that launched the completed .NET check can retain an
  // image handle until its child bookkeeping unwinds. Clean from a short-lived
  // sibling process so the isolated output is still gone before this check exits.
  const cleanup = spawnSync(process.execPath, ['-e',
    "require('node:fs').rmSync(process.argv[1],{recursive:true,force:true,maxRetries:12,retryDelay:250})", output],
    { encoding: 'utf8', windowsHide: true, timeout: 30_000 });
  if (cleanup.error) throw cleanup.error;
  if (cleanup.status !== 0) throw new Error(`Phone runtime cleanup failed (${cleanup.status}):\n${cleanup.stderr}`);
}
