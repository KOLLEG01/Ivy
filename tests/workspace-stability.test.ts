import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { resolve, join } from 'node:path';
import { spawnSync } from 'node:child_process';

const instant = (seconds: number) => new Date(Date.UTC(2026, 8, 7, 0, 0, seconds)).toISOString();
const observation = (seconds: number, bootId = 'process-one') => ({
  instanceId: 'manager', observedAt: instant(seconds), ownerBootId: 'owner-one', ownerPid: 40,
  targetRevision: 'launch-one', candidateId: 'candidate-one', buildId: 'build-one', state: 'ready', restartCount: 0,
  health: { ready: true, bootId, pid: 50, launchId: 'launch-one', observedAt: instant(seconds) },
});
const inputs = () => ({
  activation: { hostId: 'test-acceptance', phase: 'ready', startedAt: instant(0), completedAt: instant(61),
    eventsBefore: { max: 0, oom: 0, oom_kill: 0, oom_group_kill: 0 },
    resourcesAfter: { 'memory.events': 'max 0\noom 0\noom_kill 0\noom_group_kill 0' },
    firstReadyBoots: { manager: { ownerBootId: 'owner-one', ownerPid: 40, targetRevision: 'launch-one',
      candidateId: 'candidate-one', buildId: 'build-one', bootId: 'process-one', pid: 50 } },
    after: { instances: [{ instanceId: 'manager', enabled: true }], observations: { manager: observation(60) } } },
  history: { at: instant(62), exits: { manager: [] as { at: string; targetRevision: string; exitCode: number; signal: null; forced: boolean }[] } },
  samples: [1, 30, 60].map(seconds => ({ at: instant(seconds), observations: { manager: observation(seconds) } })),
});
async function assess(value: ReturnType<typeof inputs>): Promise<{ exit: number | null; report: { accepted: boolean; findings: { code: string }[]; sampleCount: number } }> {
  const root = await mkdtemp(join(tmpdir(), 'ivy-stability-'));
  try {
    await writeFile(join(root, 'activation.json'), JSON.stringify(value.activation));
    await writeFile(join(root, 'history.json'), JSON.stringify(value.history));
    await writeFile(join(root, 'samples.jsonl'), value.samples.map(row => JSON.stringify(row)).join('\n') + '\n');
    const child = spawnSync(process.execPath, [resolve('tests/acceptance/scenarios/workspace-stability.mjs'),
      '--activation', join(root, 'activation.json'), '--history', join(root, 'history.json'),
      '--samples', join(root, 'samples.jsonl'), '--output', join(root, 'result.json')], { encoding: 'utf8', timeout: 15000 });
    assert.equal(child.error, undefined, child.stderr); assert.equal(child.signal, null);
    return { exit: child.status, report: JSON.parse(await readFile(join(root, 'result.json'), 'utf8')) };
  } finally { await rm(root, { recursive: true, force: true }); }
}

test('startup assessment accepts unchanged ready owners and rejects an intermediate boot hidden by final readiness', async () => {
  const stable = await assess(inputs()); assert.equal(stable.exit, 0); assert.equal(stable.report.accepted, true); assert.equal(stable.report.sampleCount, 3);
  const changed = inputs(); changed.samples[1]!.observations.manager = observation(30, 'restarted-process');
  const failed = await assess(changed); assert.equal(failed.exit, 1); assert.equal(failed.report.accepted, false);
  assert.deepEqual(failed.report.findings.map(row => row.code), ['process_identity_changed']);
});

test('a reset backoff counter cannot hide a recorded selected-launch exit; unrelated historical exits do not fail startup', async () => {
  const value = inputs(); value.history.exits.manager.push({ at: instant(20), targetRevision: 'previous-launch', exitCode: 0, signal: null, forced: false });
  assert.equal((await assess(value)).report.accepted, true);
  value.history.exits.manager.push({ at: instant(25), targetRevision: 'launch-one', exitCode: 0, signal: null, forced: false });
  const failed = await assess(value); assert.equal(failed.exit, 1);
  assert.deepEqual(failed.report.findings.map(row => row.code), ['selected_launch_exited']);
  value.history.exits.manager.pop(); value.activation.resourcesAfter['memory.events'] = 'max 1\noom 0\noom_kill 0\noom_group_kill 0';
  assert.deepEqual((await assess(value)).report.findings.map(row => row.code), ['memory_event']);
});
