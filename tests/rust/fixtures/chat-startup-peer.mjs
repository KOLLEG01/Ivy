import { once } from 'node:events';
import { createInterface } from 'node:readline';
import { WebSocketServer } from 'ws';
import './chat-store-loader.mjs';

// Load the public contract hash without booting the original service oracle.
const { toolDefinitionHash } = await import('../../../packages/contracts/src/tool-definition.ts');
const server = new WebSocketServer({ host: '127.0.0.1', port: 0, perMessageDeflate: false });
await once(server, 'listening');
const url = `http://127.0.0.1:${server.address().port}/ivy/`;
let state, request = 0;
const sessions = new Map(), pending = new Map();
const begin = () => state = { generation: 6, calls: [], heartbeats: [], handshakes: [], mismatches: [] };
begin();
const send = (socket, frame) => socket.send(JSON.stringify(frame));
const failure = code => ({ code: -32000, message: code, data: { code, outcome: 'unknown' } });
server.on('connection', socket => {
  const selected = state, session = { generation: ++selected.generation };
  sessions.set(socket, session);
  socket.on('close', () => sessions.delete(socket));
  socket.on('message', bytes => {
    const frame = JSON.parse(bytes);
    if (!frame.method) {
      pending.get(frame.id)?.(frame);
      pending.delete(frame.id);
      return;
    }
    const { id, method, params } = frame, call = { method, params };
    selected.calls.push(call);
    let value;
    if (method === 'service.connect') value = { generation: session.generation };
    else if (method === 'registry.sync') { session.registry = params; value = {}; }
    else if (method === 'notifications.subscribe') value = {};
    else if (method === 'service.heartbeat') {
      selected.heartbeats.push({ generation: session.generation, ...params });
      value = { ready: params.ready };
    } else if (method === 'serviceNodes.get') value = { principalId: 'chat-service', serviceName: 'chat-bridge', connected: true, synced: true };
    else if (method === 'system.status') value = { runtimeEpoch: 'chat' };
    else if (method === 'system.diagnostics') value = { items: [], nextCursor: null };
    else if (method === 'settings.get') value = { settings: { quietHours: null } };
    else {
      if (method !== 'tools.list') selected.mismatches.push(call);
      send(socket, { jsonrpc: '2.0', id, error: failure(method === 'tools.list' ? 'fixture_native_offline' : 'fixture_rpc_mismatch') });
      return;
    }
    if (['service.connect', 'registry.sync', 'notifications.subscribe'].includes(method))
      selected.handshakes.push({ generation: session.generation, method, params });
    call.value = value;
    send(socket, { jsonrpc: '2.0', id, result: value });
  });
});
console.log(JSON.stringify({ url }));
for await (const line of createInterface({ input: process.stdin, crlfDelay: Infinity })) {
  const command = JSON.parse(line);
  let value;
  if (command.action === 'begin') { begin(); value = null; }
  else if (command.action === 'summary') value = state;
  else if (command.action === 'invoke') {
    const [socket, session] = [...sessions].find(([, s]) => s.generation === command.generation);
    const name = command.name ?? 'workspace', id = 'startup-invoke-' + (++request);
    const definition = session.registry.namespaces[0].tools.find(tool => tool.name === name);
    const reply = new Promise(resolve => pending.set(id, resolve));
    send(socket, { jsonrpc: '2.0', id, method: 'provider.invoke', params: {
      qualifiedName: 'chat.' + name, arguments: command.arguments ?? {},
      definitionHash: toolDefinitionHash(definition), generation: session.generation, callerPrincipalId: 'alice',
    } });
    value = await reply;
  } else throw Error('Unknown startup command');
  console.log(JSON.stringify({ value }));
}
