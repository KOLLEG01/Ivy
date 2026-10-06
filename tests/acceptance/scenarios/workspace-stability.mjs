import assert from 'node:assert/strict';
import { readFile, writeFile } from 'node:fs/promises';
import { createHash } from 'node:crypto';
import { parseArgs } from 'node:util';
import { resolve } from 'node:path';

// Read-only assessment of an original measured activation and its independently retained history.
// restartCount is a consecutive-failure/backoff counter and can reset; it is not a lifetime count.
const { values } = parseArgs({ options: Object.fromEntries(['activation', 'history', 'samples', 'output'].map(name => [name, { type: 'string' }])) });
assert.ok(values.activation && values.history && values.output, 'Use --activation, --history and a new --output path.');
const load = async path => {
  const bytes = await readFile(resolve(path)); assert.ok(bytes.length <= 4 * 1024 * 1024);
  return { value: JSON.parse(bytes), hash: 'sha256:' + createHash('sha256').update(bytes).digest('hex') };
};
const activation = await load(values.activation), history = await load(values.history), a = activation.value;
const start = Date.parse(a.startedAt), end = Date.parse(a.completedAt);
assert.ok(Number.isFinite(start) && Number.isFinite(end) && end >= start);
assert.ok(history.value.exits && Date.parse(history.value.at) >= end, 'History must cover the complete activation.');
const events = Object.fromEntries(a.resourcesAfter['memory.events'].trim().split('\n').map(line => {
  const [key, value] = line.trim().split(/\s+/); return [key, Number(value)];
}));
const findings = [], observations = a.after.observations;
let samplesHash = null, sampleCount = 0;
const bootSamples = new Map();
if (a.firstReadyBoots) assert.ok(values.samples, 'An activation with boot tracking also needs its original --samples.');
if (values.samples) {
  const bytes = await readFile(resolve(values.samples)); assert.ok(bytes.length > 0 && bytes.length <= 64 * 1024 * 1024);
  samplesHash = 'sha256:' + createHash('sha256').update(bytes).digest('hex');
  const samples = bytes.toString('utf8').trim().split('\n').map(line => JSON.parse(line)); sampleCount = samples.length;
  assert.ok(sampleCount > 1 && sampleCount <= 10_000);
  let prior = start;
  for (const sample of samples) {
    const at = Date.parse(sample.at); assert.ok(Number.isFinite(at) && at >= prior && at >= start && at <= end); prior = at;
    assert.ok(sample.observations && typeof sample.observations === 'object');
    for (const [key, row] of Object.entries(sample.observations)) {
      const observedAt = Date.parse(row.observedAt); assert.ok(Number.isFinite(observedAt));
      if (observedAt < start) continue;
      assert.ok(observedAt <= at + 1000);
      const seen = bootSamples.get(key) ?? { owners: new Set(), processes: new Set(), targets: new Set(), readyCount: 0 };
      assert.ok(typeof row.ownerBootId === 'string' && Number.isInteger(row.ownerPid));
      seen.owners.add(JSON.stringify([row.ownerBootId, row.ownerPid]));
      if (row.state === 'ready' && row.health?.ready && at - observedAt <= 15000) {
        assert.ok(typeof row.health.bootId === 'string' && Number.isInteger(row.health.pid));
        seen.processes.add(JSON.stringify([row.health.bootId, row.health.pid]));
        seen.targets.add(JSON.stringify([row.targetRevision, row.candidateId, row.buildId])); seen.readyCount++;
      }
      bootSamples.set(key, seen);
    }
  }
  assert.ok(end - prior <= 15000, 'The samples must cover the end of the activation.');
}
if (a.phase !== 'ready') findings.push({ code: 'controller_not_ready', phase: a.phase });
for (const key of ['max', 'oom', 'oom_kill', 'oom_group_kill']) {
  assert.ok(Number.isInteger(events[key]) && Number.isInteger(a.eventsBefore[key]));
  if (events[key] !== a.eventsBefore[key]) findings.push({ code: 'memory_event', event: key, before: a.eventsBefore[key], after: events[key] });
}
assert.ok(a.after.instances.length > 0);
for (const instance of a.after.instances) {
  const key = instance.instanceId, observation = observations[key];
  assert.ok(observation && Array.isArray(history.value.exits[key]), 'Every expected owner needs an observation and exit history.');
  assert.ok(Number.isFinite(Date.parse(observation.observedAt)), 'Final observation needs a valid timestamp.');
  assert.ok(history.value.exits[key].every(exit => Number.isFinite(Date.parse(exit.at))), 'Exit evidence needs valid timestamps.');
  if (!instance.enabled || observation.state !== 'ready' || end - Date.parse(observation.observedAt) > 15000 || Date.parse(observation.observedAt) > end + 1000 || !observation.health?.ready) {
    findings.push({ code: 'final_readiness_missing', instanceId: key });
  }
  const exits = history.value.exits[key].filter(exit => Date.parse(exit.at) >= start && Date.parse(exit.at) <= end && exit.targetRevision === observation.targetRevision);
  for (const exit of exits) findings.push({ code: 'selected_launch_exited', instanceId: key, at: exit.at, targetRevision: exit.targetRevision, exitCode: exit.exitCode, signal: exit.signal, forced: exit.forced });
  if (values.samples) {
    const seen = bootSamples.get(key), first = a.firstReadyBoots?.[key];
    if (!seen || seen.readyCount < 2) findings.push({ code: 'ready_boot_samples_missing', instanceId: key });
    else {
      if (seen.owners.size !== 1 || !seen.owners.has(JSON.stringify([observation.ownerBootId, observation.ownerPid]))) findings.push({ code: 'owner_identity_changed', instanceId: key });
      if (seen.processes.size !== 1 || !seen.processes.has(JSON.stringify([observation.health?.bootId, observation.health?.pid]))) findings.push({ code: 'process_identity_changed', instanceId: key });
      if (seen.targets.size !== 1 || !seen.targets.has(JSON.stringify([observation.targetRevision, observation.candidateId, observation.buildId]))) findings.push({ code: 'ready_target_changed', instanceId: key });
    }
    if (a.firstReadyBoots && (!first || first.ownerBootId !== observation.ownerBootId || first.ownerPid !== observation.ownerPid ||
      first.targetRevision !== observation.targetRevision || first.candidateId !== observation.candidateId || first.buildId !== observation.buildId ||
      first.bootId !== observation.health?.bootId || first.pid !== observation.health?.pid)) findings.push({ code: 'first_ready_identity_mismatch', instanceId: key });
  }
}
const result = { schemaVersion: 1, kind: 'original-workspace-activation-stability-assessment', hostId: a.hostId,
  startedAt: a.startedAt, completedAt: a.completedAt, activationHash: activation.hash, historyHash: history.hash,
  ...(values.samples ? { samplesHash, sampleCount } : {}),
  accepted: findings.length === 0, findings, observedAt: new Date().toISOString(),
  scope: 'Final ready state, unchanged hard-limit/OOM counters, and no recorded exit of the selected launches within the original activation window.' +
    (values.samples ? ' Retained samples also require unchanged owner, process and ready target identities.' : '') + ' Does not establish later load or user-flow capacity.' };
await writeFile(resolve(values.output), JSON.stringify(result, null, 2) + '\n', { flag: 'wx' });
console.log(JSON.stringify(result)); if (!result.accepted) process.exitCode = 1;
