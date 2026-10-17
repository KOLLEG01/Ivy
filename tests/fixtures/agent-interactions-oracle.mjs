import { NativeInteractions } from '../../dist/services/agent-manager/src/interactions.js';
import { IvyError } from '../../dist/packages/contracts/src/errors.js';

// No native process, account, or network: exercise the unchanged TS cache.
let now = 100, calls = 0;
Date.now = () => now;
const cache = new NativeInteractions({ request: async () => {
  calls++;
  if (calls === 1) throw new IvyError('native_capacity', 'Busy');
  if (calls === 2) throw new IvyError('outcome_unknown', 'Lost', 'unknown');
  return { result: '\ud800' };
}}, 'epoch', { count: 3, bytes: 1024, ttlMs: 100 });
const result = [];
async function observe(action) {
  try { result.push(await action()); }
  catch (e) { result.push({ code: e.code, outcome: e.outcome }); }
}
await observe(() => cache.run('c', 'one', 'm', null));
await observe(() => cache.run('c', 'one', 'm', null));
await observe(() => cache.run('c', 'one', 'm', null));
await observe(() => cache.lookup('other', 'one'));
now = 200;
await observe(() => cache.lookup('c', 'one'));
await observe(() => cache.run('c', 'one', 'm', null));
await observe(() => cache.run('c', 'two', 'm', null));
await observe(() => cache.lookup('c', 'one'));
await observe(() => cache.run('c', 'three', 'm', null));
await observe(() => cache.run('c', 'four', 'm', null));
await observe(() => cache.lookup('c', 'two'));
console.log(JSON.stringify({ result, calls }));
