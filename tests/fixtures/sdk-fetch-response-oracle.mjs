import {registerHooks} from 'node:module';
import {existsSync, readFileSync} from 'node:fs';
import {fileURLToPath} from 'node:url';
import {createHash} from 'node:crypto';
import net from 'node:net';

registerHooks({resolve(specifier, context, next) {
  if (specifier.startsWith('.') && specifier.endsWith('.js') && context.parentURL) {
    const source = new URL(specifier.slice(0, -3) + '.ts', context.parentURL);
    if (existsSync(fileURLToPath(source))) return next(source.href, context);
  }
  return next(specifier, context);
}});
const {HiveClient} = await import('../../packages/sdk/src/client.ts');
const {IvyError} = await import('../../packages/contracts/src/errors.ts');
const sha = bytes => createHash('sha256').update(bytes).digest('hex');
const placeholder = '00000000-0000-0000-0000-000000000000';
const cases = [];
for (const status of [200, 201, 202, 204, 205, 206, 299, 300, 301, 302, 303, 304,
  305, 306, 307, 308, 309, 399, 400, 401, 403, 404, 409, 418, 500, 503, 599]) {
  cases.push({name: 'status-' + status, status});
}
for (const status of [200, 204, 205, 300, 301, 302, 303, 304, 305, 306, 307, 308, 309, 399, 404]) {
  cases.push({name: 'location-' + status, status, headers: [['Location', '/must-not-follow']]});
}
for (const status of [204, 205, 304]) {
  cases.push({name: 'empty-' + status, status, empty: true});
  cases.push({name: 'invalid-utf8-' + status, status, invalidUtf8: true});
  cases.push({name: 'invalid-gzip-' + status, status, headers: [['Content-Encoding', 'gzip']]});
}
cases.push(
  {name: 'empty-200', status: 200, empty: true},
  {name: 'one-bom', status: 200, bom: 1},
  {name: 'two-boms', status: 200, bom: 2},
  {name: 'invalid-utf8', status: 200, invalidUtf8: true},
  {name: 'ignored-content-type', status: 200, headers: [['Content-Type', 'text/plain; charset=iso-8859-1']]},
  {name: 'chunked', status: 200, chunked: true},
  {name: 'chunked-invalid-utf8', status: 200, chunked: true, invalidUtf8: true},
  {name: 'truncated-length', status: 200, lengthDelta: 1},
  {name: 'truncated-chunked', status: 200, chunked: true, cutTail: 5},
  {name: 'interim-103', status: 200, interim: 103},
  {name: 'interim-100', status: 200, interim: 100},
  {name: 'upgrade-101', status: 101, headers: [['Connection', 'Upgrade'], ['Upgrade', 'fixture']]},
  {name: 'json-extra-data', status: 200, extra: 'trailing'},
);
function wire(spec, id) {
  const normal = Buffer.from(JSON.stringify({jsonrpc: '2.0', id,
    result: {name: spec.name, text: 'é😀', raw: '\ud800'}}));
  let body = spec.empty ? Buffer.alloc(0) : normal;
  if (spec.invalidUtf8) {
    body = Buffer.from(normal); body[body.indexOf(Buffer.from('é'))] = 0xff;
  }
  if (spec.bom) body = Buffer.concat([...Array(spec.bom).fill(Buffer.from([239, 187, 191])), body]);
  if (spec.extra) body = Buffer.concat([body, Buffer.from(spec.extra)]);
  const fields = [...(spec.headers ?? []),
    ...(!spec.headers?.some(([name]) => name.toLowerCase() === 'connection') ? [['Connection', 'close']] : []),
    ...(spec.chunked ? [['Transfer-Encoding', 'chunked']] : [['Content-Length', '' + (body.length + (spec.lengthDelta ?? 0))]])];
  const interim = spec.interim ? `HTTP/1.1 ${spec.interim} Fixture\r\n\r\n` : '';
  const head = Buffer.from(interim + `HTTP/1.1 ${spec.status} Fixture\r\n` + fields.map(([k,v]) => k + ': ' + v + '\r\n').join('') + '\r\n');
  if (spec.chunked) {
    const chunks = [];
    for (let at = 0; at < body.length;) {
      const end = Math.min(body.length, at === 0 ? 64 : at + 1), part = body.subarray(at, end);
      chunks.push(Buffer.from(part.length.toString(16) + '\r\n'), part, Buffer.from('\r\n')); at = end;
    }
    body = Buffer.concat([...chunks, Buffer.from('0\r\n\r\n')]);
  }
  let bytes = Buffer.concat([head, body]);
  if (spec.cutTail) bytes = bytes.subarray(0, bytes.length - spec.cutTail);
  return bytes;
}
const sockets = new Set(), observedRequests = [];
const server = net.createServer(socket => {
  sockets.add(socket); socket.on('close', () => sockets.delete(socket)); socket.on('error', () => {});
  let buffer = Buffer.alloc(0), replied = false;
  socket.on('data', part => {
    if (replied) throw Error('Unexpected request replay or redirect');
    buffer = Buffer.concat([buffer, part]);
    const end = buffer.indexOf('\r\n\r\n'); if (end < 0) return;
    const head = buffer.subarray(0, end).toString('latin1').split('\r\n');
    const length = Number(head.find(line => line.toLowerCase().startsWith('content-length:'))?.split(':')[1]);
    if (!Number.isSafeInteger(length)) throw Error('Missing request length');
    if (buffer.length < end + 4 + length) return;
    const frame = JSON.parse(buffer.subarray(end + 4, end + 4 + length).toString('utf8'));
    const name = frame.params.name, spec = cases.find(row => row.name === name);
    if (!spec) throw Error('Unknown case');
    const bytes = wire(spec, frame.id);
    observedRequests.push({name, method: frame.method, params: frame.params, path: head[0].split(' ')[1],
      headers: head.slice(1).map(line => line.toLowerCase().startsWith('host:') ? 'host: <loopback>' : line),
      responseByteLength: bytes.length, responseSha256: sha(bytes)});
    replied = true; socket.end(bytes);
  });
});
await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
const base = 'http://127.0.0.1:' + server.address().port;
const rows = [];
try {
  for (const spec of cases) {
    const start = observedRequests.length;
    let fetchResponse = null, result;
    const client = new HiveClient(base + '/case/' + rows.length, {credential: 'fixture', fetch: async (...args) => {
      const response = await fetch(...args);
      fetchResponse = {status: response.status, bodyNull: response.body === null};
      return response;
    }});
    try { result = {ok: true, value: await client.request('fixture.echo', {name: spec.name}, {timeoutMs: 3000})}; }
    catch (error) { result = {ok: false, error: IvyError.from(error).toWire()}; }
    const template = wire(spec, placeholder), idOffset = template.indexOf(placeholder);
    rows.push({spec, wireTemplateHex: template.toString('hex'), idOffset, fetchResponse, result, requests: observedRequests.slice(start)});
  }
} finally {
  for (const socket of sockets) socket.destroy();
  await new Promise(resolve => server.close(resolve));
}
const source = readFileSync(new URL('../../packages/sdk/src/client.ts', import.meta.url));
process.stdout.write(JSON.stringify({node: process.version, undici: process.versions.undici,
  sourceSha256: sha(source), transport: 'Actual original HiveClient and Node global fetch against loopback TCP bytes; transparent observation only.', rows}));
