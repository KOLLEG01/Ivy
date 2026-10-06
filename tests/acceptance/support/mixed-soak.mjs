import assert from 'node:assert/strict';
import { readFile, appendFile } from 'node:fs/promises';
import { join } from 'node:path';
import { freemem, totalmem } from 'node:os';
import { setTimeout as delay } from 'node:timers/promises';
import { chromium } from '@playwright/test';
import { discover, callBound } from '../../../dist/packages/sdk/src/client.js';
import { atomicJson } from '../../../dist/packages/host-runtime/src/config.js';
import { hashJson } from '../../../dist/packages/contracts/src/canonical.js';

// Adds bounded background traffic to an existing actual-model scenario. It never
// starts or retries a model turn, owns no external account and changes only its own object.
export async function startMixedSoak({ profilePath, root, client, baseUrl, token, owner, taskBoardNode, id }) {
  assert.ok(process.execArgv.includes('--max-old-space-size=2048'), 'Start the mixed owner with its explicit 2048 MiB Node heap limit');
  const profile = JSON.parse(await readFile(profilePath, 'utf8'));
  assert.equal(profile.schemaVersion, 1);
  assert.ok(Number.isSafeInteger(profile.durationMs) && profile.durationMs >= 60000 && profile.durationMs <= 7200000);
  assert.ok(Number.isSafeInteger(profile.intervalMs) && profile.intervalMs >= 1000 && profile.intervalMs <= 30000);
  assert.equal(profile.browserContexts, 2); assert.equal(profile.writers, 1);
  assert.equal(profile.maximumSampleMs, 30000); assert.equal(profile.maximumRootRssBytes, 3 * 1024 ** 3);
  const count = Math.ceil(profile.durationMs / profile.intervalMs);
  assert.ok(count <= 1440);
  const reportPath = join(root, 'mixed-soak.json'), samplesPath = join(root, 'mixed-soak-samples.jsonl');
  const report = { schemaVersion: 1, phase: 'starting', profile, profileHash: hashJson(profile), maximumIterations: count,
    ownModelTurns: 0, completedSamples: 0, successfulWrites: 0, browserReads: 0, failure: null,
    memoryScope: 'RSS/heap and CPU of the shared Hive/AgentManager/TaskBoard process; host free RAM. Browser and native child memory are not separately attributed.',
    hardware: { totalMemoryBytes: totalmem() } };
  await atomicJson(reportPath, report);
  const binding = await discover(client, 'agent.status', { serviceNodeId: owner });
  const status = () => callBound(client, binding, {}, undefined, { timeoutMs: profile.maximumSampleMs });
  const original = await status(); assert.equal(original.state, 'ready');
  const originalTaskBoard = await client.request('serviceNodes.get', { serviceNodeId: taskBoardNode });
  assert.ok(originalTaskBoard.connected && originalTaskBoard.synced && originalTaskBoard.ready);
  const generations = new Set([originalTaskBoard.generation]); let outageStarted = null;
  report.observedRecoveryMs = [];
  const key = 'acceptance/mixed-soak';
  await client.request('contracts.register', { mutationId: id + '-soak-contract', definition: { key, version: '1.0.0', owner: { kind: 'agent' },
    mediaType: 'application/json', specMarkdown: 'Synthetic sequence owned by one bounded mixed acceptance run.',
    jsonSchema: { type: 'object', additionalProperties: false, properties: { sequence: { type: 'integer', minimum: 0 } }, required: ['sequence'] } } });
  const created = await client.request('objects.write', { mutationId: id + '-soak-create', contractVersion: '1.0.0',
    create: { contractKey: key, parentId: null, name: 'Mixed soak ' + id }, content: { encoding: 'json', value: { sequence: 0 } } });
  const objectId = created.object.id; let revision = created.revision.revision;
  const browser = await chromium.launch({ ...(process.platform === 'win32' ? { channel: 'msedge' } : {}), headless: true });
  const pages = [], browserErrors = [], outsideRequests = [], abort = new AbortController();
  const timings = [], rss = [];
  let stopped = false;
  const save = () => atomicJson(reportPath, report);
  try {
    for (let index = 0; index < profile.browserContexts; index++) {
      const context = await browser.newContext(); context.setDefaultTimeout(30000);
      await context.route('**/*', route => { if (new URL(route.request().url()).origin !== new URL(baseUrl).origin) { outsideRequests.push(route.request().url()); return route.abort(); } return route.continue(); });
      const page = await context.newPage(); page.on('pageerror', error => browserErrors.push(error.message));
      await page.goto(baseUrl + '/'); await page.getByLabel('Hive credential').fill(token);
      const login = page.waitForResponse(response => response.request().method() === 'POST' && response.url() === baseUrl + '/login');
      await page.getByRole('button', { name: 'Sign in', exact: true }).click(); assert.equal((await login).status(), 303);
      await page.waitForURL(url => url.pathname === new URL(baseUrl + '/').pathname); pages.push(page);
    }
  } catch (error) {
    report.phase = 'failed'; report.failure = { code: error.code ?? error.name, message: error.message }; report.finishedAt = new Date().toISOString();
    try { await browser.close(); } finally { await save(); }
    throw error;
  }
  const started = Date.now(); report.startedAt = new Date(started).toISOString(); report.phase = 'running'; report.objectId = objectId;
  await save();
  const completion = (async () => {
    try {
      for (let index = 1; index <= count; index++) {
        if (stopped) throw new Error('Mixed soak interrupted before its fixed duration');
        assert.ok(Date.now() <= started + profile.durationMs + profile.maximumSampleMs, 'Mixed soak exceeded its bounded wall time');
        const sampleStarted = performance.now();
        const written = await client.request('objects.write', { mutationId: id + '-soak-write-' + index, objectId, expectedRevision: revision,
          contractVersion: '1.0.0', content: { encoding: 'json', value: { sequence: index } } }, { timeoutMs: profile.maximumSampleMs });
        assert.equal(written.object.id, objectId); assert.equal(written.revision.revision, revision + 1); revision++; report.successfulWrites++;
        const reads = await Promise.all(pages.map(page => page.evaluate(async ({ objectId, sequence }) => {
          const started = performance.now();
          const response = await fetch('./api/v1/rpc', { method: 'POST', headers: { 'Content-Type': 'application/json' }, credentials: 'same-origin',
            signal: AbortSignal.timeout(30000), body: JSON.stringify({ jsonrpc: '2.0', id: crypto.randomUUID(), method: 'objects.read', params: { objectId } }) });
          const frame = await response.json();
          if (!response.ok || frame.error || frame.result?.content?.value?.sequence !== sequence) throw new Error('Mixed browser observation did not match its original write');
          return { elapsedMs: performance.now() - started, revision: frame.result.revision.revision };
        }, { objectId, sequence: index })));
        for (const read of reads) assert.equal(read.revision, revision); report.browserReads += reads.length;
        const agent = await status(); assert.equal(agent.state, 'ready'); assert.equal(agent.epoch, original.epoch); assert.equal(agent.pid, original.pid);
        const service = await client.request('serviceNodes.get', { serviceNodeId: taskBoardNode }, { timeoutMs: profile.maximumSampleMs });
        assert.equal(service.principalId, originalTaskBoard.principalId);
        generations.add(service.generation); assert.ok(generations.size <= 2, 'Only the scenario-controlled TaskBoard restart is allowed');
        if (!service.connected || !service.synced || !service.ready) {
          outageStarted ??= Date.now(); assert.ok(Date.now() - outageStarted <= 30000, 'TaskBoard did not recover within the fixed interval');
        } else if (outageStarted !== null) { report.observedRecoveryMs.push(Date.now() - outageStarted); outageStarted = null; }
        assert.ok(agent.pendingInputs <= 16); assert.ok(agent.operations.retained <= agent.operations.maximum && agent.operations.bytes <= agent.operations.maximumBytes);
        const memory = process.memoryUsage(), elapsedMs = performance.now() - sampleStarted;
        report.lastObservation = { elapsedMs, memory, pendingInputs: agent.pendingInputs, operations: agent.operations };
        assert.ok(memory.rss <= profile.maximumRootRssBytes, 'Mixed root process exceeded its fixed RSS acceptance threshold');
        assert.ok(elapsedMs <= profile.maximumSampleMs, 'Mixed sample exceeded its fixed deadline');
        assert.deepEqual(browserErrors, []); assert.deepEqual(outsideRequests, []);
        await appendFile(samplesPath, JSON.stringify({ index, at: new Date().toISOString(), elapsedMs, reads, memory, usage: process.resourceUsage(), freeMemoryBytes: freemem(),
          pendingInputs: agent.pendingInputs, operations: agent.operations, nativePid: agent.pid, nativeEpoch: agent.epoch,
          taskBoard: { generation: service.generation, connected: service.connected, synced: service.synced, ready: service.ready } }) + '\n', { mode: 0o600 });
        report.completedSamples++; await save();
        timings.push(elapsedMs); rss.push(memory.rss);
        const remaining = started + Math.min(index * profile.intervalMs, profile.durationMs) - Date.now();
        if (remaining > 0) await delay(remaining, undefined, { signal: abort.signal });
      }
      assert.ok(Date.now() - started >= profile.durationMs); assert.equal(outageStarted, null);
      assert.equal(generations.size, 2, 'Observe the controlled restart and no further service generations');
      report.serviceGenerations = [...generations]; report.phase = 'passed';
    } catch (error) { report.phase = 'failed'; report.failure = { code: error.code ?? error.name, message: error.message }; }
    finally {
      report.finishedAt = new Date().toISOString(); report.elapsedMs = Date.now() - started;
      if (timings.length) {
        const sorted = [...timings].sort((a, b) => a - b), mean = timings.reduce((sum, value) => sum + value, 0) / timings.length;
        report.sampleTimingMs = { count: timings.length, mean, p50: sorted[Math.ceil(sorted.length * 0.5) - 1], p95: sorted[Math.ceil(sorted.length * 0.95) - 1],
          max: sorted.at(-1), populationStandardDeviation: Math.sqrt(timings.reduce((sum, value) => sum + (value - mean) ** 2, 0) / timings.length) };
        report.rootRssBytes = { first: rss[0], last: rss.at(-1), peak: Math.max(...rss), change: rss.at(-1) - rss[0] };
      }
      try { await browser.close(); } catch (error) { report.phase = 'failed'; report.cleanupError = error.code ?? error.message; }
      await save();
    }
    return report;
  })();
  return { completion, reportPath, stop: async () => { stopped = true; abort.abort(); return completion; } };
}
