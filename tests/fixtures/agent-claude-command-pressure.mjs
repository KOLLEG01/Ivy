// Hermetic pipe peer: no SDK, credentials, model work or network access.
import { existsSync } from 'node:fs';
const held = [];
const turns = new Set(), ready = new Map();
const reply = (id, result) => process.stdout.write(JSON.stringify({ version: 2, id, result }) + '\n');
const release = () => {
  const entries = held.splice(0);
  for (const entry of entries) reply(entry.id, entry.tag);
  return entries;
};
let buffer = '', timer;
process.stdin.on('data', chunk => {
  buffer += chunk;
  let end;
  while ((end = buffer.indexOf('\n')) >= 0) {
    const frame = JSON.parse(buffer.slice(0, end));
    buffer = buffer.slice(end + 1);
    const params = frame.params ?? {};
    if (frame.method === 'event/ack') continue;
    if (frame.method === 'fixture/replySize') {
      if (params.fail) process.stdout.write(JSON.stringify({ version: 2, id: frame.id,
        error: { name: 'fixture_error', message: `${'x'.repeat(params.units)}original-${params.tag}-${frame.id}\ud800` } }) + '\n');
      else reply(frame.id, { originalId: frame.id, body: 'x'.repeat(params.units), tag: params.tag,
        ...(params.extraUnits ? { extraBody: 'x'.repeat(params.extraUnits) } : {}) });
      continue;
    }
    if (frame.method === 'fixture/wrongVersion') {
      process.stdout.write(JSON.stringify({ version: 1, id: frame.id, result: null }) + '\n');
      continue;
    }
    if (frame.method === 'fixture/awaitTurn') {
      if (turns.has(params.turnId)) reply(frame.id, null);
      else ready.set(params.turnId, frame.id);
    } else if (frame.method === 'query/start') {
      turns.add(params.turnId);
      if (ready.has(params.turnId)) {
        reply(ready.get(params.turnId), null);
        ready.delete(params.turnId);
      }
      reply(frame.id, null);
    } else if (frame.method === 'fixture/permission') {
      process.stdout.write(JSON.stringify({ version: 2, eventSequence: 1, type: 'permission', callbackId: 1,
        turnId: params.turnId, toolName: 'Bash', input: { command: 'fixture' }, toolUseId: 'original-tool' }) + '\n');
      reply(frame.id, null);
    } else if (frame.method === 'fixture/hold' || params.hold) {
      held.push({ id: frame.id, tag: params.tag });
    } else if (frame.method === 'permission/reply') {
      reply(frame.id, release());
    } else if (frame.method === 'fixture/pauseInput') {
      process.stdin.pause();
      timer = setInterval(() => {
        if (existsSync(params.release)) release();
        if (existsSync(params.resume)) {
          clearInterval(timer);
          process.stdin.resume();
        }
      }, 10);
      reply(frame.id, null);
    } else {
      reply(frame.id, params);
    }
  }
});
process.stdin.on('end', () => { clearInterval(timer); });
