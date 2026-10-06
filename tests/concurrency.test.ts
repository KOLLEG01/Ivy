import test from 'node:test';
import assert from 'node:assert/strict';
import { mapConcurrent } from '../packages/host-runtime/src/concurrency.js';

test('bounded owned work overlaps independent operations but preserves input order', async () => {
  const first = Promise.withResolvers<void>(), second = Promise.withResolvers<void>(), third = Promise.withResolvers<void>();
  const admitted: number[] = [], waiting = [first, second, third];
  const work = mapConcurrent([0, 1, 2], 2, async index => {
    admitted.push(index); await waiting[index]!.promise; return 'result-' + index;
  });
  assert.deepEqual(admitted, [0, 1]);
  second.resolve(); await second.promise; await Promise.resolve();
  assert.deepEqual(admitted, [0, 1, 2]);
  third.resolve(); first.resolve();
  assert.deepEqual(await work, ['result-0', 'result-1', 'result-2']);
  assert.deepEqual(await mapConcurrent([], 2, async () => assert.fail('Empty work cannot run.')), []);
});

test('a failed owned operation stops admission and waits for all active operations before rejecting', async () => {
  const failed = Promise.withResolvers<void>(), active = Promise.withResolvers<void>(), admitted: number[] = [];
  const original = new Error('First operation failed.'), later = new Error('Other active operation failed.');
  let settled = false;
  const work = mapConcurrent([0, 1, 2, 3], 2, async index => {
    admitted.push(index); await (index === 0 ? failed : active).promise;
  });
  const outcome = work.then(() => assert.fail('Failure must reject.'), error => { settled = true; assert.equal(error, original); });
  failed.reject(original); await failed.promise.catch(() => {}); await Promise.resolve();
  assert.deepEqual(admitted, [0, 1]); assert.equal(settled, false);
  active.reject(later); await outcome;
  assert.deepEqual(admitted, [0, 1]); assert.equal(settled, true);
});

test('synchronous and undefined failures remain failures, and invalid bounds admit no work', async () => {
  let calls = 0;
  await assert.rejects(mapConcurrent([0, 1], 2, () => { calls++; throw new Error('Synchronous failure'); }), /Synchronous failure/);
  assert.equal(calls, 1);
  let rejected = false;
  await mapConcurrent([0], 2, async () => { throw undefined; }).then(() => assert.fail('Undefined rejection must fail.'), error => { rejected = true; assert.equal(error, undefined); });
  assert.equal(rejected, true);
  for (const bound of [0, 9, 1.5, NaN, Infinity]) await assert.rejects(mapConcurrent([0], bound, async () => assert.fail('Invalid bound cannot admit work.')), { code: 'invalid_arguments' });
});
