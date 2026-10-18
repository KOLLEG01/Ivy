// Actual reference Hive transport, routing and storage over synthetic temp data.
// Launch with data-collector-source-loader.mjs imported in this process/workers.
import { once } from 'node:events';
import { createServer } from 'node:net';
import { join } from 'node:path';
import { createInterface } from 'node:readline';
import { HiveServer } from '../../services/hive/src/server.ts';
import { digest } from '../../packages/contracts/src/canonical.ts';

const reservation = createServer().listen(0, '127.0.0.1');
await once(reservation, 'listening');
const port = reservation.address().port;
await new Promise(resolve => reservation.close(resolve));
const url = `http://127.0.0.1:${port}/ivy/`;
const server = new HiveServer({
  filename: join(process.argv[2], 'hive.sqlite'),
  publicBaseUrl: url,
  credentials: [{ principalId: 'fixture', digest: digest('synthetic-only') }],
  version: 'fixture',
  buildId: digest('collector-transfer-fixture'),
  listenHost: '127.0.0.1',
  listenPort: port,
  validateMessages: true,
});
try {
  await server.start();
  console.log(JSON.stringify({ url }));
  for await (const line of createInterface({ input: process.stdin })) {
    if (line === 'close') break;
    throw new Error('Unknown Collector Hive fixture control.');
  }
} finally {
  await server.close();
}
