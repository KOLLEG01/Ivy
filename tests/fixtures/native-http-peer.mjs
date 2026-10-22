// Raw loopback HTTP/1 peer for native client tests. This never implements a client.
import net from 'node:net';
import tls from 'node:tls';
import fs from 'node:fs';

const secure = process.argv[2] === 'tls';
const host = process.argv[3] ?? '127.0.0.1';
if (!['127.0.0.1', '127.0.0.2', '::1'].includes(host)) throw Error('Loopback only');
let sequence = 0;
let connection = 0;
const records = new Map();
const sockets = new Set();
const timers = new Set();
const log = (event, fields = {}) => process.stdout.write(JSON.stringify({sequence: ++sequence, event, ...fields}) + '\n');
function identity(socket) {
  const key = `${socket.remoteAddress}:${socket.remotePort}`;
  if (!records.has(key)) throw Error('Missing raw connection identity');
  return records.get(key);
}
function connected(socket) {
  const key = `${socket.remoteAddress}:${socket.remotePort}`;
  const id = ++connection;
  records.set(key, id);
  sockets.add(socket);
  log('connection', {id, localAddress: socket.localAddress, localPort: socket.localPort, remoteAddress: socket.remoteAddress, remotePort: socket.remotePort});
  socket.on('error', error => log('socket-error', {id, code: error.code}));
  socket.on('close', hadError => {sockets.delete(socket); if (records.get(key) === id) records.delete(key); log('close', {id, hadError});});
}
function handle(socket) {
  const id = identity(socket);
  if (secure) log('secure', {id, alpn: socket.alpnProtocol, servername: socket.servername, protocol: socket.getProtocol(), cipher: socket.getCipher().standardName});
  let buffered = Buffer.alloc(0);
  let index = 0;
  const write = bytes => {
    if (socket.destroyed) return;
    const buffer = Buffer.from(bytes);
    log('write', {id, index, length: buffer.length, hex: buffer.toString('hex')});
    socket.write(buffer);
  };
  const later = (delay, bytes) => {
    const timer = setTimeout(() => {timers.delete(timer); write(bytes);}, delay);
    timers.add(timer);
    socket.once('close', () => {clearTimeout(timer); timers.delete(timer);});
  };
  socket.on('data', bytes => {
    buffered = Buffer.concat([buffered, bytes]);
    log('read', {id, length: bytes.length, hex: bytes.toString('hex')});
    while (true) {
      const end = buffered.indexOf('\r\n\r\n');
      if (end < 0) return;
      const head = buffered.subarray(0, end + 4).toString('latin1');
      const length = Number(/\r\ncontent-length:\s*(\d+)/i.exec(head)?.[1] ?? 0);
      if (buffered.length < end + 4 + length) return;
      const request = buffered.subarray(0, end + 4 + length);
      buffered = buffered.subarray(request.length);
      index++;
      const path = head.split(' ')[1];
      const mode = path.split('?')[0].slice(1);
      log('request', {id, index, mode, path, head, hex: request.toString('hex')});
      const body = `reply-${id}-${index}`;
      const headers = `X-Connection-Id: ${id}\r\nX-Request-Index: ${index}\r\n`;
      const final = `HTTP/1.1 200 OK\r\n${headers}Content-Length: ${body.length}\r\n\r\n${body}`;
      switch (mode) {
        case 'stall-head': break;
        case 'stall-body': write(`HTTP/1.1 200 OK\r\n${headers}Content-Length: 9\r\n\r\nstart`); break;
        case 'truncated': write(`HTTP/1.1 200 OK\r\n${headers}Content-Length: 9\r\n\r\nshort`); socket.end(); break;
        case 'peer-close': write(`HTTP/1.1 200 OK\r\n${headers}Connection: close\r\nContent-Length: ${body.length}\r\n\r\n${body}`); socket.end(); break;
        case 'chunked': write(`HTTP/1.1 200 OK\r\n${headers}Transfer-Encoding: chunked\r\nTrailer: X-Finished\r\n\r\n3\r\nabc\r\n0\r\nX-Finished: yes\r\n\r\n`); break;
        case 'large-trailer': write(`HTTP/1.1 200 OK\r\n${headers}Transfer-Encoding: chunked\r\n\r\n3\r\nabc\r\n0\r\nX-Large: ${'x'.repeat(1024)}\r\n\r\n`); break;
        case 'large-head': write(`HTTP/1.1 200 OK\r\n${headers}X-Large: ${'x'.repeat(1024)}\r\nContent-Length: ${body.length}\r\n\r\n${body}`); break;
        case 'many-head': write(`HTTP/1.1 200 OK\r\n${headers}${Array.from({length: 32}, (_, i) => `X-${i}: x\r\n`).join('')}Content-Length: ${body.length}\r\n\r\n${body}`); break;
        case 'keepalive-zero': write(`HTTP/1.1 200 OK\r\n${headers}Keep-Alive: timeout=0\r\nContent-Length: ${body.length}\r\n\r\n${body}`); break;
        case 'upgrade': write(`HTTP/1.1 101 Switching Protocols\r\n${headers}Connection: Upgrade\r\nUpgrade: fixture\r\n\r\nforbidden`); break;
        case '100-coalesced': write('HTTP/1.1 100 Continue\r\n\r\n' + final); break;
        case '100-only': write('HTTP/1.1 100 Continue\r\n\r\n'); break;
        case '100-partial-final': write('HTTP/1.1 100 Continue\r\n\r\nHTTP/1.1 200 O'); break;
        case '100-after-103': write('HTTP/1.1 103 Early Hints\r\nLink: </early>\r\n\r\nHTTP/1.1 100 Continue\r\n\r\n' + final); break;
        case '100-split': {
          write('HTTP/1.1 100 Continue\r\n\r\n');
          later(100, final);
          break;
        }
        case 'segmented':
          write(`HTTP/1.1 200 OK\r\n${headers}Content-Len`);
          later(10, 'gth: 6\r\n\r\nabc');
          later(20, 'def');
          break;
        case 'info': write([102, 103, 104, 199].map(status => `HTTP/1.1 ${status} Informational\r\nX-Info: ${status}\r\n\r\n`).join('') + final); break;
        default: write(final);
      }
    }
  });
}
const server = secure ? tls.createServer({
  cert: fs.readFileSync(new URL('./native-http-server-cert.pem', import.meta.url)),
  // Public fixture key for the untrusted test CA; never a deployment identity.
  key: fs.readFileSync(new URL('./native-http-server-key.pem', import.meta.url)),
  ALPNProtocols: ['http/1.1'],
}, handle) : net.createServer();
server.on('connection', connected);
if (!secure) server.on('connection', handle);
server.on('tlsClientError', error => log('tls-error', {code: error.code}));
server.on('error', error => {log('server-error', {code: error.code}); process.exitCode = 1;});
server.listen(0, host, () => log('listening', {host, port: server.address().port, secure}));
process.stdin.resume();
function stop() {
  for (const timer of timers) clearTimeout(timer);
  for (const socket of sockets) socket.destroy();
  server.close();
}
process.stdin.on('end', stop);
