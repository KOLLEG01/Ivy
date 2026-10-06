import test from 'node:test';
import assert from 'node:assert/strict';
import { digest } from '../packages/contracts/src/canonical.js';
import { createSystemdAdapter, parseSystemdState, renderSystemdUnit, systemdUnitName } from '../packages/host-runtime/src/systemd.js';
import type { Host } from '../packages/contracts/src/generated.js';

const config = {
  hostId: 'linux-systemd-fixture', runtimeRoot: '/var/lib/ivy', executables: { node: '/usr/bin/node' },
  instances: [{ instanceId: 'service', serviceNodeId: 'service-node', componentId: 'task-board', enabled: true, engine: 'process', settings: {} }],
} as unknown as Host.HostConfig;
const buildId = digest('build');
const candidate: Host.Candidate = { candidateId: buildId, componentId: 'task-board', artifactRoot: '/srv/artifact',
  manifestPath: '/srv/artifact/component.json', createdAt: new Date().toISOString(), platform: { os: 'linux', arch: 'x64', node: '24.18.0' },
  buildId };
const target: Host.RuntimeTarget = { schemaVersion: 1, instanceId: 'service', revision: '11111111-1111-4111-8111-111111111111',
  candidateId: candidate.candidateId, desired: 'running', configPath: '/var/lib/ivy/instances/service/config.json', requestedAt: new Date().toISOString() };
const manifest: Host.ReleaseManifest = { schemaVersion: 1, componentId: 'task-board', kind: 'service', version: '0.1.0', connectsToHive: true,
  requirements: { node: '>=24.18.0 <25.0.0', hiveProtocol: 1 as const, contracts: [] }, entrypoint: { executable: 'node', args: ['dist/services/task-board/src/main.js'], timeoutMs: 30_000 },
  readiness: { timeoutMs: 30_000, command: { executable: 'node', args: ['dist/packages/host-runtime/src/health.js'], timeoutMs: 5_000 } },
  shutdown: { timeoutMs: 5_000 }, restart: { policy: 'always', minimumDelayMs: 100, maximumDelayMs: 5_000 }, buildId };

test('systemd state parsing keeps only bounded lifecycle identity', () => {
  assert.deepEqual(parseSystemdState('LoadState=loaded\nActiveState=active\nSubState=running\nMainPID=42\n'),
    { loadState: 'loaded', activeState: 'active', subState: 'running', mainPid: 42 });
  assert.equal(parseSystemdState('LoadState=not-found\nActiveState=inactive\nSubState=dead\nMainPID=0\n').mainPid, null);
});

test('dynamic systemd units bind one exact target revision and never invoke the removed guardian', () => {
  const rendered = renderSystemdUnit(config, 'service', target, candidate, manifest);
  assert.equal(rendered.unit, systemdUnitName(config, 'service'));
  assert.match(rendered.content, /ExecStart=.*dist\/services\/task-board\/src\/main\.js.*--config/);
  assert.match(rendered.content, /Environment=IVY_LAUNCH_ID=11111111-1111-4111-8111-111111111111/);
  assert.doesNotMatch(rendered.content, /guardian/);
});

test('systemd adapter proves a stop before allowing a replacement owner to start', async () => {
  const unit = systemdUnitName(config, 'service'), calls: string[] = [], outputs = [
    'LoadState=loaded\nActiveState=active\nSubState=running\nMainPID=42\n',
    'LoadState=loaded\nActiveState=inactive\nSubState=dead\nMainPID=0\n',
  ];
  const adapter = createSystemdAdapter(async (_config, args) => {
    calls.push(args.join(' '));
    return args[0] === 'show' ? outputs.shift() ?? 'LoadState=loaded\nActiveState=inactive\nSubState=dead\nMainPID=0\n' : '';
  });
  const stopped = await adapter.stopUnit(config, unit, 1000);
  assert.equal(stopped.activeState, 'inactive');
  assert.deepEqual(calls, [`show --no-pager --property=LoadState,ActiveState,SubState,MainPID ${unit}`, `stop ${unit}`,
    `show --no-pager --property=LoadState,ActiveState,SubState,MainPID ${unit}`]);
});
