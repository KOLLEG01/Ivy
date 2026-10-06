import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { isAbsolute, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { jsonFile } from '../../dist/packages/host-runtime/src/config.js';
import { HiveClient, serviceTools } from '../../dist/packages/sdk/src/client.js';
import { setupAudioHardware } from '../../dist/services/phone-bridge/src/runtime/audio-hardware-setup.js';
import { validatePhoneService } from '../../dist/services/phone-bridge/src/runtime/registry.js';

assert.equal(process.platform, 'win32');
assert.equal(process.argv.length, 3, 'Usage: node tools/operations/setup-phone-audio-hardware.mjs <absolute-request.json>');
assert.ok(isAbsolute(process.argv[2]));
const request = await jsonFile(process.argv[2], 32768);
assert.ok(isAbsolute(request.credentialPath));
const secret = await jsonFile(request.credentialPath, 32768);
assert.equal(typeof secret.credential, 'string');
const tools = serviceTools(new HiveClient(request.publicBaseUrl, { credential: secret.credential }), request.serviceNodeId,
  [{ namespace: 'phone', interfaceVersion: '1.0.0' }]);
const backend = fileURLToPath(new URL('./phone-audio-hardware.ps1', import.meta.url));
assert.ok(process.env.SystemRoot && isAbsolute(process.env.SystemRoot));
const powershell = join(process.env.SystemRoot, 'System32/WindowsPowerShell/v1.0/powershell.exe');
async function invoke(command) {
  return await new Promise((resolve, reject) => {
    const child = spawn(powershell, ['-NoProfile', '-NonInteractive', '-File', backend], { windowsHide: true, stdio: ['pipe', 'pipe', 'pipe'] });
    let output = '', bytes = 0, timedOut = false;
    const timeout = setTimeout(() => { timedOut = true; child.kill(); }, 60000);
    child.stdout.on('data', chunk => { bytes += chunk.length; if (bytes > 65536) child.kill(); else output += String(chunk); });
    child.stderr.resume(); // Never echo untrusted OS data or credentials as a setup exception.
    child.stdin.on('error', () => undefined);
    child.once('error', error => { clearTimeout(timeout); reject(error); });
    child.once('exit', code => {
      clearTimeout(timeout);
      if (code !== 0 || timedOut || bytes > 65536) return reject(new Error('Original hardware command failed or has an unknown outcome.'));
      try { resolve(JSON.parse(output)); } catch (error) { reject(error); }
    });
    child.stdin.end(JSON.stringify(command));
  });
}
const result = await setupAudioHardware({ operationId: request.operationId, instanceIds: request.instanceIds,
  receiptPath: request.receiptPath, ...(request.apply === undefined ? {} : { apply: request.apply }) }, {
  async readiness() {
    const status = await tools.call('phone.status', {}), setup = await tools.call('phone.audioSetup', {});
    validatePhoneService('PhoneServiceStatus', status); validatePhoneService('PhoneAudioSetup', setup);
    return { busy: status.busy, ready: setup.routes.every(route => route.ready),
      endpointIds: [...new Set(setup.routes.flatMap(route => route.endpoints.map(endpoint => endpoint.id)))].sort() };
  },
  inspect(instanceIds, protectedEndpointIds) { return invoke({ action: 'inspect', instanceIds, protectedEndpointIds }); },
  async disable(target, protectedEndpointIds) {
    const result = await invoke({ action: 'disable', instanceIds: [target.instanceId], protectedEndpointIds, expected: target });
    assert.equal(result.length, 1); assert.equal(result[0].instanceId, target.instanceId); assert.equal(result[0].disabled, true);
  },
});
process.stdout.write(JSON.stringify(result, null, 2) + '\n');
