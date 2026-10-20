// This entry is only launched with its immutable SDK payload by the Rust owner.
import { readFile } from 'node:fs/promises';
import { session } from './session.mjs';
const metadata = JSON.parse(await readFile(new URL('../sdk/package.json', import.meta.url), 'utf8'));
if (metadata.name !== '@anthropic-ai/claude-agent-sdk' || metadata.version !== '0.3.283')
  throw new Error('SDK payload identity mismatch');
const sdk = await import('../sdk/sdk.mjs');
const bridge = session(sdk, value => process.stdout.write(JSON.stringify(value) + '\n'));
const maximumBytes = 32 * 1024 * 1024;
let buffered = Buffer.alloc(0);
const pending = new Set();
try {
  for await (const chunk of process.stdin) {
    buffered = buffered.length ? Buffer.concat([buffered, chunk]) : chunk;
    let end;
    while ((end = buffered.indexOf(10)) >= 0) {
      if (end > maximumBytes) throw new Error('SDK bridge frame exceeds byte limit');
      const line = buffered.subarray(0, end);
      buffered = buffered.subarray(end + 1);
      if (!line.length) continue;
      const action = bridge.receive(JSON.parse(line.toString('utf8')));
      pending.add(action);
      void action.finally(() => pending.delete(action));
    }
    if (buffered.length > maximumBytes) throw new Error('SDK bridge frame exceeds byte limit');
  }
  if (buffered.length) throw new Error('SDK bridge input ended mid-frame');
} finally {
  await bridge.stop();
  await Promise.allSettled(pending);
}
