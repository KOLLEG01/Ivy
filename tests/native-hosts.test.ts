import test from 'node:test';
import assert from 'node:assert/strict';
import type { Operation, RpcClient } from '../packages/sdk/src/client.js';
import { nativeHosts, nativeServiceNodes } from '../packages/ui-client/src/native-hosts.js';

test('native host discovery shares a complete bounded inventory and groups distinct services by physical host', async () => {
  const nodes = [{ serviceNodeId: 'host.first', hostId: 'host', connected: true, ready: false },
    { serviceNodeId: 'host.second', hostId: 'host', connected: true, ready: true }];
  const calls: Record<string, unknown>[] = [];
  const client = { async request(_method: string, params: Record<string, unknown>) {
    calls.push(params); return { items: [nodes[params['cursor'] ? 1 : 0]], nextCursor: params['cursor'] ? null : 'next' };
  } } as unknown as RpcClient;
  const found = await nativeServiceNodes(client, undefined, 'host');
  assert.deepEqual(found, nodes); assert.equal(calls[0]!['hostId'], 'host');
  assert.deepEqual(nativeHosts(found), [{ hostId: 'host', serviceNodeId: 'host.second', connected: true, ready: true, nodes }]);
});
test('native host discovery refuses cycles and excessive empty pages instead of returning partial owners', async () => {
  let calls = 0, cycle = true;
  const client = { async request() { calls++; return { items: [] as Operation.ServiceNode[], nextCursor: cycle ? 'same' : 'empty-' + calls }; } } as unknown as RpcClient;
  await assert.rejects(nativeServiceNodes(client), { code: 'native_hosts_unavailable' }); assert.equal(calls, 2);
  cycle = false; calls = 0;
  await assert.rejects(nativeServiceNodes(client), { code: 'native_hosts_unavailable' }); assert.equal(calls, 20);
});
test('host load shows used shares, keeps unknown values unknown and never presents stale readings as current', async () => {
  const { accountGroups, metricsStale, sharedRead, systemMeters } = await import('../packages/ui-client/src/host-load.js');
  const observedAt = new Date('2026-11-11T10:00:00.000Z').toISOString();
  const metrics = { observedAt, sampleSeconds: 55, memory: { totalBytes: 16, usedBytes: 12 },
    cpu: { usedPercent: null, averageSeconds: 300, coveredSeconds: 0 },
    volumes: [{ path: '/srv/projects', volume: '/srv', totalBytes: 100, usedBytes: 71 }] };
  const meters = systemMeters(metrics);
  assert.deepEqual(meters.map(meter => [meter.caption, meter.used]), [['RAM', 75], ['CPU', null], ['Disk', 71]]);
  assert.match(meters[2]!.detail!, /\/srv · \/srv\/projects$/);
  const warm = systemMeters({ ...metrics, cpu: { usedPercent: 37.5, averageSeconds: 300, coveredSeconds: 120 } })[1]!;
  assert.deepEqual([warm.used, warm.detail], [38, 'average of the last 2 min (warming up to 5 min)']);
  assert.equal(systemMeters({ ...metrics, memory: null, cpu: null, volumes: [] }).every(meter => meter.used === null), true);
  assert.equal(metricsStale(metrics, Date.parse(observedAt) + 179000), false);
  assert.equal(metricsStale(metrics, Date.parse(observedAt) + 181000), true);

  const codex = { rateLimits: { limitId: 'codex', primary: { usedPercent: 40, windowDurationMins: 300 }, secondary: { usedPercent: 12.4, windowDurationMins: 10080 } },
    rateLimitsByLimitId: { codex: { limitId: 'codex', limitName: 'Codex', primary: { usedPercent: 40, windowDurationMins: 300 }, secondary: { usedPercent: 12.4, windowDurationMins: 10080 } },
      other: { limitId: 'other', primary: { usedPercent: 99, windowDurationMins: 300 } },
      seat: { limitId: 'seat', individualLimit: { remainingPercent: 30, used: '7', limit: '10' } } } };
  const claude = { rateLimits: { limitId: 'claude-code', primary: { windowDurationMins: 300 }, secondary: null } };
  const groups = accountGroups([
    { key: 'a', label: 'Codex · main (codex)', ready: true, result: codex, error: null },
    { key: 'b', label: 'Claude · default (claude)', ready: true, result: claude, error: null },
    { key: 'c', label: 'Codex · spare (codex-2)', ready: false, result: null, error: null },
  ]);
  // The current bucket's windows are shown; explicit remaining shares are inverted only in their own bucket.
  assert.deepEqual(groups.map(group => [group.caption, group.meters.map(meter => [meter.caption, meter.used])]),
    [['Codex main', [['5h', 40], ['7d', 12]]], ['Claude', [['', null]]], ['Codex spare', [['', null]]]]);
  assert.equal(groups[1]!.meters[0]!.detail, 'no reading yet');
  assert.equal(groups[2]!.meters[0]!.detail, 'account not ready');
  const { usageBuckets } = await import('../packages/ui-client/src/host-load.js');
  assert.deepEqual(usageBuckets(codex).find(bucket => bucket.id === 'seat')!.windows.map(window => window.used), [70]);

  const storage = new Map<string, string>();
  Object.assign(globalThis, { localStorage: { getItem: (key: string) => storage.get(key) ?? null, setItem: (key: string, value: string) => storage.set(key, value) } });
  let loads = 0;
  const load = async () => ++loads;
  assert.equal(await sharedRead('host', 55000, load), 1);
  assert.equal(await sharedRead('host', 55000, load), 1, 'another tab reuses the recent reading');
  assert.equal(await sharedRead('host', 55000, load, true), 2, 'an explicit refresh reads again');
  storage.set('ivy.shared-read:host', JSON.stringify({ at: Date.now() - 56000, value: 9 }));
  assert.equal(await sharedRead('host', 55000, load), 3);
});
