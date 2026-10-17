// Runs the unchanged WorkerClient with deterministic time and a message-port double.
// Node's built-in TypeScript transform avoids installing/building the TS backend.
import { readFileSync } from 'node:fs';
import { stripTypeScriptTypes } from 'node:module';
import { EventEmitter } from 'node:events';

const errors = stripTypeScriptTypes(readFileSync(new URL('../../packages/contracts/src/errors.ts', import.meta.url), 'utf8'), { mode: 'transform' });
const IvyError = new Function(errors.replaceAll('export ', '') + '\nreturn IvyError;')();
const source = stripTypeScriptTypes(readFileSync(new URL('../../services/hive/src/worker-client.ts', import.meta.url), 'utf8'), { mode: 'transform' })
  .replace(/^import .*;$/gm, '')
  .replaceAll('export ', '')
  .replaceAll('import.meta.url', JSON.stringify(new URL('../../services/hive/src/worker-client.ts', import.meta.url).href));
let now = 0, timerId = 0;
const timers = new Map();
const setTimer = (callback, ms) => { const id = ++timerId; timers.set(id, { at: now + (ms >= 1 && ms <= 2147483647 ? ms : 1), callback }); return id; };
const advance = value => {
  const until = now + value;
  for (;;) {
    const next = [...timers].filter(([,timer]) => timer.at <= until).sort((a,b) => a[1].at - b[1].at || a[0] - b[0])[0];
    if (!next) break;
    timers.delete(next[0]); now = next[1].at; next[1].callback();
  }
  now = until;
};
class Port extends EventEmitter {
  postMessage() {}
  terminate() { return Promise.resolve(0); }
}
const WorkerClient = new Function('Worker', 'IvyError', 'setTimeout', 'clearTimeout', source + '\nreturn WorkerClient;')(Port, IvyError, setTimer, id => timers.delete(id));
let port;
const client = new WorkerClient({}, () => (port = new Port()));
const settled = [];
const track = (promise, id) => promise.then(value => settled.push({id,value}), error => settled.push({id,error:error.toWire()}));
const flush = async () => { for(let i=0;i<5;i++) await Promise.resolve(); };
const snapshots = [];
for (const step of JSON.parse(readFileSync(0, 'utf8'))) {
  switch(step.op) {
    case 'start': { const promise = client.start(); port.emit('message', {ready:true}); await promise; break; }
    case 'post': track(client.request({action:step.backup?'backup':'health'}, step.timeout, step.terminate, step.bytes), step.id); break;
    case 'started': port.emit('message', {id:step.id,started:true}); break;
    case 'progress': port.emit('message', {id:step.id,progress:true}); break;
    case 'reply': port.emit('message', {id:step.id,value:step.value}); break;
    case 'advance': advance(step.ms); break;
    default: throw new Error('Unknown trace step');
  }
  await flush();
  snapshots.push({ready:client.ready,count:client.pending.size,bytes:client.queuedBytes,settled:[...settled]});
}
process.stdout.write(JSON.stringify(snapshots));
