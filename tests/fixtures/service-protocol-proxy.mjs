// Controlled registry/heartbeat faults on a real native service's Hive connection.
// Every ordinary frame still travels between the native owner and the real Hive.
import { createServer, request as httpRequest } from 'node:http';
import { once } from 'node:events';
import { randomUUID } from 'node:crypto';
import WebSocket, { WebSocketServer } from 'ws';

export async function serviceProtocolProxy(base, { onRequest, onResponse } = {}) {
  const target = new URL(base), sockets = new Set(), pending = new Map();
  let upstream, registry;
  const server = createServer((incoming, outgoing) => {
    const forward = httpRequest(new URL(incoming.url, target), {
      method: incoming.method, headers: { ...incoming.headers, host: target.host },
    }, response => { outgoing.writeHead(response.statusCode, response.headers); response.pipe(outgoing); });
    forward.on('error', () => outgoing.destroy());
    incoming.pipe(forward);
  });
  const wss = new WebSocketServer({ server });
  wss.on('connection', (downstream, request) => {
    const url = new URL(request.url, target); url.protocol = 'ws:';
    const headers = { authorization: request.headers.authorization, origin: target.origin };
    const peer = new WebSocket(url, { headers, maxPayload: 32 * 1024 * 1024 });
    sockets.add(downstream); sockets.add(peer);
    const queued = [];
    const requests = new Map();
    downstream.on('message', bytes => {
      const frame = JSON.parse(bytes.toString());
      onRequest?.(frame);
      if (onResponse && frame.method && frame.id !== undefined) requests.set(frame.id, frame);
      if (frame.method === 'registry.sync') registry = structuredClone(frame.params);
      if (peer.readyState === WebSocket.OPEN) peer.send(bytes, { binary: false }); else queued.push(bytes);
    });
    peer.on('open', () => { upstream = peer; for (const bytes of queued) peer.send(bytes, { binary: false }); });
    peer.on('message', bytes => {
      const frame = JSON.parse(bytes.toString()), result = pending.get(frame.id);
      if (result) {
        pending.delete(frame.id);
        frame.error ? result.reject(Error(JSON.stringify(frame.error))) : result.resolve(frame.result);
      } else if (downstream.readyState === WebSocket.OPEN) {
        const request = frame.method ? undefined : requests.get(frame.id);
        if (!frame.method) requests.delete(frame.id);
        const changed = request && onResponse?.(frame, request);
        downstream.send(changed ? JSON.stringify(changed) : bytes, { binary: false });
      }
    });
    for (const [socket, other] of [[peer, downstream], [downstream, peer]]) {
      socket.on('error', () => other.terminate());
      socket.on('close', () => { sockets.delete(socket); other.terminate(); });
    }
  });
  server.listen(0, '127.0.0.1'); await once(server, 'listening');
  const request = async (method, params) => {
    if (!upstream || upstream.readyState !== WebSocket.OPEN) throw Error('The native service is not connected.');
    const id = 'fixture-' + randomUUID();
    let timer;
    try {
      return await new Promise((resolve, reject) => {
        timer = setTimeout(() => { pending.delete(id); reject(Error('Fixture service request timed out: ' + method)); }, 15000);
        pending.set(id, { resolve, reject });
        upstream.send(JSON.stringify({ jsonrpc: '2.0', id, method, params }));
      });
    } finally { clearTimeout(timer); }
  };
  return {
    base: `http://127.0.0.1:${server.address().port}${target.pathname}`,
    heartbeat: params => request('service.heartbeat', params),
    registryChange: async change => {
      if (!registry) throw Error('The native service has not registered.');
      const next = structuredClone(registry); change(next);
      await request('registry.sync', next);
      await request('service.heartbeat', { ready: true, diagnostics: [] });
    },
    close: async () => {
      for (const socket of sockets) socket.terminate();
      for (const call of pending.values()) call.reject(Error('Fixture protocol proxy closed.'));
      pending.clear();
      await new Promise(resolve => wss.close(resolve));
      await new Promise(resolve => server.close(resolve));
    },
  };
}
