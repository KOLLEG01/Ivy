// Owned loopback service-protocol peer. No Hive backend or provider is loaded.
import {WebSocketServer} from 'ws';
import {once} from 'node:events';
import {existsSync, readFileSync, renameSync, writeFileSync} from 'node:fs';
import {join} from 'node:path';
import {createHash} from 'node:crypto';
import {createServer} from 'node:http';

const root = process.argv[2];
const save = (name, value) => {
  const path = join(root, name);
  writeFileSync(path + '.tmp', JSON.stringify(value));
  renameSync(path + '.tmp', path);
};
const canonical = value => JSON.stringify(value, (_, value) =>
  value && !Array.isArray(value) && typeof value === 'object'
    ? Object.fromEntries(Object.keys(value).sort().map(key => [key, value[key]])) : value);
let node = {connected:false,synced:false,ready:false};
const server = createServer(async (request, response) => {
  if (request.method !== 'POST' || request.url !== '/ivy/api/v1/rpc' || request.headers.authorization !== 'Bearer runtime-fixture-token') {
    response.writeHead(404).end(); return;
  }
  let body = '';
  for await (const part of request) body += part;
  const frame = JSON.parse(body);
  if (frame.method !== 'serviceNodes.get' || frame.params.serviceNodeId !== 'runtime-fixture.agent') {
    response.writeHead(400).end(); return;
  }
  const override = join(root, 'registration-override.json');
  const result = {...node, ...(existsSync(override) ? JSON.parse(readFileSync(override,'utf8')) : {})};
  response.writeHead(200, {'content-type':'application/json'}).end(JSON.stringify({jsonrpc:'2.0',id:frame.id,result}));
});
const wss = new WebSocketServer({server});
server.listen(0, '127.0.0.1');
await once(server, 'listening');
console.log(JSON.stringify({url:`http://127.0.0.1:${server.address().port}/ivy/`}));
wss.on('connection', (socket, request) => {
  let registry, invoked = false;
  const send = frame => socket.send(JSON.stringify(frame));
  const reply = (id, result) => send({jsonrpc:'2.0', id, result});
  save('connection.json', {authorization:request.headers.authorization, path:request.url});
  socket.on('message', bytes => {
    const frame = JSON.parse(bytes.toString());
    if (!frame.method) {
      if (frame.id === 'status-probe') save('status-reply.json', frame);
      return;
    }
    const {id, method, params} = frame;
    if (method === 'service.connect') {
      node = {...params, connected:true,synced:false,ready:false};
      save('identity.json', params);
      reply(id, {generation:1});
    } else if (method === 'registry.sync') {
      node.synced = true;
      registry = params;
      save('registry.json', params);
      reply(id, {});
    } else if (method === 'system.status') reply(id, {runtimeEpoch:'runtime-fixture'});
    else if (method === 'tools.call') send({jsonrpc:'2.0', id, error:{code:-32000,message:'The provider is reconciling.',data:{code:'service_not_ready',outcome:'not_executed',details:null}}});
    else if (method === 'service.heartbeat') {
      node.ready = params.ready;
      reply(id, {ready:params.ready});
      if (params.ready && !invoked) {
        invoked = true;
        const namespace = registry.namespaces.find(namespace => namespace.tools.some(tool => tool.name === 'status'));
        if (!namespace) throw new Error('Agent status tool was not registered');
        const tool = namespace.tools.find(tool => tool.name === 'status');
        const {discovery, ...definition} = tool;
        const definitionHash = 'sha256:' + createHash('sha256').update(canonical(definition)).digest('hex');
        send({jsonrpc:'2.0',id:'status-probe',method:'provider.invoke',params:{qualifiedName:`${namespace.namespace}.status`,arguments:{},definitionHash,generation:1,callerPrincipalId:'runtime-fixture-caller',operationId:'runtime-fixture-status'}});
      }
    } else if (id !== undefined) reply(id, {});
  });
});
