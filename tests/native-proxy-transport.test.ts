import test from 'node:test';
import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { connect } from 'node:net';
import { once } from 'node:events';
import { WebSocketServer } from 'ws';
import { proxyTransport } from '../services/agent-manager/src/proxy-transport.js';

test('official byte tunnel receives WebSocket upgrade and preserves JSONL messages across fragments', async t => {
  const server = createServer(), ws = new WebSocketServer({ server }); server.listen(0, '127.0.0.1'); await once(server, 'listening');
  t.after(async () => { for (const socket of ws.clients) socket.terminate(); ws.close(); await new Promise<void>(resolve => server.close(() => resolve())); });
  const accepted = once(ws, 'connection'), stream = connect((server.address() as { port: number }).port, '127.0.0.1');
  const transport = await proxyTransport(stream, stream); t.after(() => transport.close());
  const [peer] = await accepted, received = once(peer, 'message');
  transport.input.write('{"id":1,"method":"initialize"}\n');
  assert.equal(String((await received)[0]), '{"id":1,"method":"initialize"}');
  const reply = once(transport.output, 'data'); peer.send('{"id":1,', { fin: false }); peer.send('"result":{"text":"line\\nnext"}}', { fin: true });
  assert.equal(String((await reply)[0]), '{"id":1,"result":{"text":"line\\nnext"}}\n');
  const ended = once(transport.output, 'end'); peer.close(); transport.output.resume(); await ended;
});

test('official byte tunnel closes with a WebSocket handshake before destroying the byte tunnel', async t => {
  const server = createServer(), ws = new WebSocketServer({ server }); server.listen(0, '127.0.0.1'); await once(server, 'listening');
  t.after(async () => { for (const socket of ws.clients) socket.terminate(); ws.close(); await new Promise<void>(resolve => server.close(() => resolve())); });
  const accepted = once(ws, 'connection'), stream = connect((server.address() as { port: number }).port, '127.0.0.1');
  const transport = await proxyTransport(stream, stream);
  const [peer] = await accepted, closed = once(peer, 'close');
  await transport.close();
  const [code] = await closed;
  assert.equal(code, 1000);
});
