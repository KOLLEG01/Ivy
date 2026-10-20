import assert from 'node:assert/strict';
import { createRequire } from 'node:module';

const require = createRequire(import.meta.url);
const { getFutureExpirationTimestamp } = require('web-push/src/vapid-helper.js');
const RealDate = Date;
const small = process.argv.includes('--small');
const dates = small ? [
  '2026-01-01T00:00:00.123Z', '2026-03-29T00:30:45.123Z',
  '2026-10-25T01:30:45.987Z', '2011-12-29T20:30:00.001Z',
] : [
  '2026-01-01T00:00:00.123Z', '2026-07-01T00:00:00.999Z',
  '2026-03-28T16:30:45.123Z', '2026-03-29T00:30:45.987Z',
  '2026-10-24T14:30:45.001Z', '2026-10-25T01:30:45.987Z',
  '2026-03-07T19:30:45.123Z', '2026-03-08T06:30:00.001Z',
  '2026-10-31T17:30:45.987Z', '2026-11-01T06:30:00.001Z',
  '2026-10-03T05:00:00.123Z', '2026-10-03T15:15:00.987Z',
  '2026-04-04T04:00:00.001Z', '2026-04-04T15:15:00.987Z',
  '2011-12-29T20:30:00.001Z', '2011-12-30T09:30:00.123Z',
  '1900-01-01T00:00:00.123Z', '1800-01-01T00:00:00.987Z',
  '0000-01-01T00:00:00.123Z', '0099-01-01T00:00:00.987Z',
  '-000001-01-01T00:00:00.123Z', '+260000-07-01T00:00:00.987Z',
];
const times = dates.map(value => RealDate.parse(value));
if (!small) times.push(0, -0, -1, -0.9, 0.9, NaN, Infinity, -Infinity,
  8.64e15, 8.64e15 - 1, 8.64e15 + 1, -8.64e15, -8.64e15 + 1, -8.64e15 - 1);
const increments = small ? [43200, 86400, -43200, 0] : [
  43200, 86400, -43200, 0, -0, 1, -1, 0.9, -0.9, 1.2, -1.2,
  60, -60, 3600, -3600, 9e12, -9e12, 1e20, -1e20, NaN, Infinity, -Infinity,
];
const numeric = value => Number.isFinite(value) ? value : String(value);
const rows = [];
for (const now of times) for (const seconds of increments) {
  const date = new RealDate(now);
  date.setSeconds(date.getSeconds() + seconds);
  // Call the original web-push owner with its clock frozen, retaining the
  // inherited native Date methods and its floor-to-epoch-seconds behavior.
  globalThis.Date = class extends RealDate {
    constructor(...args) { super(...(args.length ? args : [now])); }
  };
  let expiration;
  try { expiration = getFutureExpirationTimestamp(seconds); }
  finally { globalThis.Date = RealDate; }
  assert.ok(Object.is(expiration, Math.floor(date.getTime() / 1000)));
  rows.push({now: numeric(now), seconds: numeric(seconds), millis: numeric(date.getTime()),
    expiration: numeric(expiration), negativeZero: Object.is(date.getTime(), -0)});
}
process.stdout.write(JSON.stringify(rows));
