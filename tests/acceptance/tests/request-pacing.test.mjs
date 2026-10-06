import assert from 'node:assert/strict';
import { test } from 'node:test';
import { requestPacer } from '../support/request-pacing.mjs';

test('concurrent clients share spaced admissions without serializing their requests', async () => {
  let now = 0;
  const pace = requestPacer(25, () => now, async ms => { now += ms; });
  const starts = [];
  await Promise.all(Array.from({ length: 100 }, async () => { await pace(); starts.push(now); }));
  assert.equal(starts.length, 100);
  for (let n = 1; n < starts.length; n++) assert.ok(starts[n] - starts[n - 1] >= 25);
});

test('a late timer does not create a catch-up burst and early timers wait again', async () => {
  let now = 0, calls = 0;
  const pace = requestPacer(25, () => now, async ms => { now += ++calls === 1 ? 1000 : ms / 2 < 1 ? ms : ms / 2; });
  await pace(); await pace();
  assert.equal(now, 1000);
  await pace();
  assert.equal(now, 1025);
});

test('invalid pacing intervals are rejected', () => {
  for (const interval of [0, -1, NaN, Infinity]) assert.throws(() => requestPacer(interval));
});
