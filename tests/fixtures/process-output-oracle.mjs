import { readFileSync } from 'node:fs';
import { stripTypeScriptTypes } from 'node:module';

// Execute the actual diagnostic algorithm without starting a process or importing
// the executor. The native Node type stripper keeps this oracle dependency-free.
const source = readFileSync(new URL('../../packages/host-runtime/src/process.ts', import.meta.url), 'utf8');
const start = source.indexOf('  const output = { stdout: Buffer.alloc(0), stderr: Buffer.alloc(0) };');
const end = source.indexOf('  child.stdout!.on(', start);
if (start < 0 || end < 0) throw new Error('Process diagnostic oracle boundary changed.');
const body = stripTypeScriptTypes(source.slice(start, end));
const capture = new Function('secrets', 'chunks', 'maxOutputBytes', `
  let ended = false, truncated = false;
  const emitted = [], options = {onOutput: (_stream, text) => emitted.push(text)};
  ${body}
  for (const chunk of chunks) received('stdout', Buffer.from(chunk, 'hex'));
  received('stdout');
  return {text: output.stdout.toString('utf8'), truncated, emitted};
`);
let input = '';
for await (const chunk of process.stdin) input += chunk;
const cases = JSON.parse(input);
process.stdout.write(JSON.stringify(cases.map(value => capture(value.secrets, value.chunks, value.maximum))));
