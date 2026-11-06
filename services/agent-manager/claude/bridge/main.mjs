import * as sdk from '@anthropic-ai/claude-agent-sdk';
import { once } from 'node:events';
import { eventFlow, session, version } from './session.mjs';

const maximumBytes = 32 * 1024 * 1024;
const output = eventFlow(async frame => {
  const bytes = Buffer.from(JSON.stringify(frame) + '\n');
  frame = undefined;
  if (bytes.length > maximumBytes) throw new Error('SDK bridge frame exceeds byte limit');
  if (!process.stdout.write(bytes)) await once(process.stdout, 'drain');
});
const bridge = session(sdk, output.send, error => process.stdin.destroy(error));
let buffer = Buffer.alloc(0);
try {
  for await (const chunk of process.stdin) {
    buffer = Buffer.concat([buffer, chunk]);
    let end;
    while ((end = buffer.indexOf(10)) >= 0) {
      if (end > maximumBytes) throw new Error('SDK bridge frame exceeds byte limit');
      const frame = JSON.parse(buffer.subarray(0, end).toString('utf8'));
      buffer = buffer.subarray(end + 1);
      if (frame.version !== version) throw new Error('SDK bridge version mismatch');
      if (frame.method === 'event/ack') { output.acknowledge(frame.params?.sequence); continue; }
      void bridge.receive(frame);
    }
    if (buffer.length > maximumBytes) throw new Error('SDK bridge frame exceeds byte limit');
  }
} finally { await bridge.stop(); output.stop(); }
