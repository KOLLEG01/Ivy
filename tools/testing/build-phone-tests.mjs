import { spawnSync } from 'node:child_process';
import { resolve } from 'node:path';
import { preparePhoneEvs, installPhoneEvs } from '../package/phone-evs-package.mjs';
const result = spawnSync(process.env.IVY_DOTNET ?? 'dotnet', ['build', 'tests/native-phone/Ivy.PhoneRuntime.Tests.csproj',
  '-p:RestoreLockedMode=true', '-p:NuGetAudit=false', '-p:UseSharedCompilation=false'], { stdio: 'inherit', windowsHide: true, timeout: 180000 });
if (result.error || result.status !== 0) throw result.error ?? new Error('Native Phone test fixture build failed.');
const root = resolve('.');
const destination = resolve('tests/native-phone/bin/Debug/net10.0-windows'), built = await preparePhoneEvs(root);
// The test output directory is reusable and may contain bytes from another EVS build. Replace
// only the files owned by this package; the ordinary installer remains create-only by default.
await installPhoneEvs(root, destination, built, { replace: true });
