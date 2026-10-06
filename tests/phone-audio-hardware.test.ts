import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, rm, readFile, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
import { randomUUID } from 'node:crypto';
import { setupAudioHardware } from '../services/phone-bridge/src/runtime/audio-hardware-setup.js';
import type { AudioHardwarePorts } from '../services/phone-bridge/src/runtime/audio-hardware-setup.js';

for (const mode of ['plan', 'complete', 'partial', 'busy', 'changed'] as const) test('hardware setup retains exact original scope: ' + mode, async t => {
  const root = await mkdtemp(join(tmpdir(), 'ivy-hardware-test-')); t.after(() => rm(root, { recursive: true, force: true }));
  const request = { operationId: randomUUID(), instanceIds: ['PCI\\first', 'HDAUDIO\\second'], receiptPath: join(root, 'receipt.json'), apply: mode !== 'plan' };
  let reads = 0; const effects: string[] = [];
  const ports: AudioHardwarePorts = {
    async readiness() { reads++; return { busy: mode === 'busy', ready: true, endpointIds: mode === 'changed' && reads > 1 ? ['changed'] : ['protected'] }; },
    async inspect(ids) { return ids.map(instanceId => ({ instanceId, name: 'Synthetic device', hardwareIds: ['fixture'], disabled: false })); },
    async disable(target) {
      const accepted = JSON.parse(await readFile(request.receiptPath, 'utf8'));
      assert.equal(accepted.state, 'accepted'); assert.deepEqual(accepted.targets.map((item: { instanceId: string }) => item.instanceId), request.instanceIds);
      effects.push(target.instanceId);
      if (mode === 'partial' && effects.length === 2) throw new Error('unknown OS outcome');
    },
  };
  if (mode === 'busy') { await assert.rejects(setupAudioHardware(request, ports)); assert.equal(effects.length, 0); return; }
  const result = await setupAudioHardware(request, ports) as { state: string };
  assert.equal(result.state, mode === 'plan' ? 'planned' : mode === 'complete' ? 'disabled' : 'outcome_unknown');
  assert.equal(effects.length, mode === 'plan' || mode === 'changed' ? 0 : 2);
  if (mode !== 'plan') {
    assert.deepEqual(await setupAudioHardware(request, ports), result); assert.equal(effects.length, mode === 'changed' ? 0 : 2);
    await assert.rejects(setupAudioHardware({ ...request, instanceIds: ['different'] }, ports), { code: 'mutation_conflict' });
  }
});

test('orphan hardware result and case-insensitive duplicate IDs refuse before device access', async t => {
  const root = await mkdtemp(join(tmpdir(), 'ivy-hardware-test-')); t.after(() => rm(root, { recursive: true, force: true }));
  const request = { operationId: randomUUID(), instanceIds: ['PCI\\first'], receiptPath: join(root, 'receipt.json'), apply: true };
  const ports: AudioHardwarePorts = {
    async readiness() { assert.fail('No device access expected'); },
    async inspect() { assert.fail('No device access expected'); },
    async disable() { assert.fail('No device access expected'); },
  };
  await assert.rejects(setupAudioHardware({ ...request, instanceIds: ['PCI\\first', 'pci\\FIRST'] }, ports), { code: 'invalid_arguments' });
  await writeFile(request.receiptPath + '.result', '{}');
  await assert.rejects(setupAudioHardware(request, ports), { code: 'mutation_conflict' });
});
