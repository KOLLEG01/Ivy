// An owned Hive process for workload measurements. The product worker, transport, SQLite
// durability and limits are unchanged. IPC controls only this acceptance process.
import { readFile } from 'node:fs/promises';
import { HiveServer } from '../../../dist/services/hive/src/server.js';

const options = JSON.parse(await readFile(process.argv[2], 'utf8'));
const server = new HiveServer(options), samples = new Map();
const request = server.worker.request.bind(server.worker);
let measuring = false;
server.worker.request = async (payload, ...args) => {
  const selected = measuring && payload.action === 'execute'
    && ['objects.list', 'objects.query'].includes(payload.request.method)
    && payload.request.params.limit === 50;
  const start = performance.now();
  try { return await request(payload, ...args); }
  finally {
    if (selected) {
      const values = samples.get(payload.request.method) ?? [];
      values.push(performance.now() - start); samples.set(payload.request.method, values);
    }
  }
};
let chain = Promise.resolve();
process.on('message', message => {
  chain = chain.then(async () => {
    try {
      let result;
      if (message.action === 'measure') { samples.clear(); measuring = true; result = { measuring }; }
      else if (message.action === 'samples') { measuring = false; result = { samples: Object.fromEntries(samples), memory: process.memoryUsage(), usage: process.resourceUsage() }; }
      else if (message.action === 'backup') result = await request({ action: 'backup', destination: message.destination }, 600_000);
      else if (message.action === 'close') { await server.close(); result = { closed: true }; }
      else throw new Error('Unknown acceptance control.');
      process.send({ id: message.id, result }, () => { if (message.action === 'close') process.disconnect(); });
    } catch (error) { process.send({ id: message.id, error: { code: error.code, message: error.message } }); }
  });
});
// Losing the harness cannot leave an acceptance listener behind.
process.on('disconnect', () => { void server.close().finally(() => process.exit()); });
await server.start();
process.send({ ready: true, pid: process.pid });
