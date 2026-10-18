import { createServer } from 'node:http';
import { connect } from 'node:net';
import { once } from 'node:events';
import { proxyTransport } from '../../dist/packages/host-runtime/src/codex-proxy-transport.js';
import { IvyError } from '../../dist/packages/contracts/src/errors.js';
const server = createServer();
server.on('upgrade', (_request, socket) => socket.end('HTTP/1.1 403 Forbidden\r\nConnection: close\r\nContent-Length: 0\r\n\r\n'));
const deadline = setTimeout(() => { throw new Error('Upgrade oracle timed out'); }, 5000);
server.listen(0, '127.0.0.1');
await once(server, 'listening');
const pipe = connect(server.address().port, '127.0.0.1');
try {
  const error = await proxyTransport(pipe, pipe).then(() => null, error => error);
  if (error === null) throw new Error('Unexpected successful upgrade');
  const { code, message, outcome } = IvyError.from(error);
  console.log(JSON.stringify({ code, message, outcome }));
} finally { server.close(); clearTimeout(deadline); }
