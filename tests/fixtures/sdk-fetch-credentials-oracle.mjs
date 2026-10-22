import {registerHooks} from 'node:module';
import {existsSync, readFileSync} from 'node:fs';
import {fileURLToPath} from 'node:url';
import net from 'node:net';
import {createHash} from 'node:crypto';
import ts from 'typescript';

registerHooks({resolve(specifier, context, next) {
  if (specifier.startsWith('.') && specifier.endsWith('.js') && context.parentURL) {
    const source = new URL(specifier.slice(0, -3) + '.ts', context.parentURL);
    if (existsSync(fileURLToPath(source))) return next(source.href, context);
  }
  return next(specifier, context);
}});
const {HiveClient} = await import('../../packages/sdk/src/client.ts');
const {IvyError} = await import('../../packages/contracts/src/errors.ts');
const source = readFileSync(new URL('../../packages/sdk/src/client.ts', import.meta.url));
const provenance = {node: process.version, undici: process.versions.undici,
  sourceSha256: createHash('sha256').update(source).digest('hex')};
const mode = process.argv[2];
if (mode === 'headers') {
  const rows = [];
  for (let unit = 0; unit < 65536; unit++) {
    try {
      const value = new Headers({Authorization: 'Bearer ' + String.fromCharCode(unit)}).get('authorization');
      rows.push({unit, bytes: Array.from(value, ch => ch.charCodeAt(0))});
    } catch {}
  }
  process.stdout.write(JSON.stringify({...provenance, rows}));
} else if (mode === 'run') {
  const input = JSON.parse(readFileSync(0, 'utf8'));
  const sockets = new Set(), requests = [];
  const server = net.createServer(socket => {
    sockets.add(socket); socket.on('close', () => sockets.delete(socket));
    socket.on('error', () => {});
    let buffer = Buffer.alloc(0), replied = false;
    socket.on('data', bytes => {
      if (replied) throw Error('Unexpected request replay');
      buffer = Buffer.concat([buffer, bytes]);
      const end = buffer.indexOf('\r\n\r\n');
      if (end < 0) return;
      const lines = buffer.subarray(0, end).toString('latin1').split('\r\n');
      const length = Number(lines.find(line => line.toLowerCase().startsWith('content-length:'))?.split(':')[1]);
      if (!Number.isSafeInteger(length)) throw Error('Missing original Content-Length');
      if (buffer.length < end + 4 + length) return;
      const authorization = lines.find(line => line.toLowerCase().startsWith('authorization:'));
      const frame = JSON.parse(buffer.subarray(end + 4, end + 4 + length).toString('utf8'));
      requests.push({path: lines[0].split(' ')[1], authorization: authorization
        ? Buffer.from(authorization.slice(authorization.indexOf(':') + 2), 'latin1').toString('hex') : null,
        headers: lines.slice(1).map(line => line.toLowerCase().startsWith('host:') ? 'host: <loopback>' : line),
        method: frame.method, params: frame.params});
      const body = JSON.stringify({jsonrpc: '2.0', id: frame.id, result: {ok: true}});
      replied = true;
      socket.end('HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: '
        + Buffer.byteLength(body) + '\r\n\r\n' + body);
    });
  });
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  const base = 'http://127.0.0.1:' + server.address().port;
  const rows = [];
  try {
    for (const row of input) {
      const start = requests.length;
      let constructed = false, result;
      try {
        const client = new HiveClient(base + '/case/' + rows.length, {credential: row.credential});
        constructed = true;
        result = {ok: true, value: await client.request('fixture.echo', {raw: '\ud800'}, {timeoutMs: 2000})};
      } catch (error) { result = {ok: false, error: IvyError.from(error).toWire()}; }
      rows.push({...row, constructed, result, requests: requests.slice(start)});
    }
  } finally {
    for (const socket of sockets) socket.destroy();
    await new Promise(resolve => server.close(resolve));
  }
  process.stdout.write(JSON.stringify({...provenance, rows}));
} else if (mode === 'reset') {
  const input = JSON.parse(readFileSync(0, 'utf8'));
  const {operationId} = await import('../../packages/contracts/src/operation-id.ts');
  const hostSource = readFileSync(new URL('../../packages/host-runtime/src/runtime-reset.ts', import.meta.url), 'utf8');
  const tree = ts.createSourceFile('runtime-reset.ts', hostSource, ts.ScriptTarget.Latest, true);
  const reset = tree.statements.find(node => ts.isFunctionDeclaration(node) && node.name?.text === 'runtimeReset');
  const declarations = reset.body.statements.filter(ts.isVariableStatement).flatMap(node => node.declarationList.declarations);
  const selected = declarations.find(node => node.name.getText(tree) === 'credential').initializer.getText(tree);
  const restore = declarations.find(node => node.name.getText(tree) === 'restoreUis').initializer;
  const initialization = restore.body.statements[0].getText(tree);
  const source = ts.transpileModule('exports.select = config => (' + selected + ');\n'
    + 'exports.restore = (config,credential,runtimeEpoch,resetId) => {' + initialization
    + ';return {base:client.base.href,mutationId,deadline};};',
    {compilerOptions: {target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.CommonJS}}).outputText;
  let trace, clock;
  class ObservedClient extends HiveClient {
    constructor(...args) { trace.push('client'); super(...args); trace.push('client-ready'); }
  }
  const observedOperationId = (...args) => { trace.push('operation-id'); return operationId(...args); };
  const reference = {};
  new Function('exports', 'HiveClient', 'operationId', 'Date', source)(reference, ObservedClient, observedOperationId,
    {now() { trace.push('now'); return clock.shift(); }});
  const capture = action => {
    try { return {ok: true, value: action()}; }
    catch (error) { return {ok: false, error: IvyError.from(error).toWire()}; }
  };
  const selections = input.selections.map(row => ({...row, result: capture(() => {
    const selected = reference.select({instances: row.instances});
    return {found: selected !== undefined, value: selected ?? null};
  })}));
  const constructors = input.constructors.map(row => {
    trace = []; clock = [...row.times];
    return {...row, result: capture(() => reference.restore({publicBaseUrl: row.base}, row.credential, row.epoch, row.resetId)), trace};
  });
  process.stdout.write(JSON.stringify({...provenance, hostSourceSha256: createHash('sha256').update(hostSource).digest('hex'),
    originalExpressions: {selected, initialization}, selections, constructors}));
} else throw Error('Unknown fetch credential oracle mode');
