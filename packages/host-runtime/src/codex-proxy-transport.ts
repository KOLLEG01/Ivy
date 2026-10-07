import { Duplex, PassThrough, Writable } from 'node:stream';
import type { Readable } from 'node:stream';
import { setTimeout as delay } from 'node:timers/promises';
import WebSocket from 'ws';
import { IvyError } from '../../sdk/src/node.js';
import { nativeFrameBytes, nativeRequestFrameBytes } from './codex-limits.js';

/** The official proxy is a byte tunnel. Its control socket carries WebSocket, not JSONL. */
export async function proxyTransport(stdin: Writable, stdout: Readable) {
  const tunnel = Duplex.from({ readable: stdout, writable: stdin });
  const socket = new WebSocket('ws://localhost/', { createConnection: () => tunnel,
    handshakeTimeout: 30000, maxPayload: nativeFrameBytes, perMessageDeflate: false });
  const output = new PassThrough();
  const input = new Writable({ write(chunk: Buffer, _encoding, done) {
    if (chunk.length > nativeRequestFrameBytes + 1 || chunk[chunk.length - 1] !== 10 || socket.readyState !== WebSocket.OPEN) {
      done(new IvyError('native_transport_invalid', 'Invalid or unavailable native proxy frame.')); return;
    }
    socket.send(chunk.subarray(0, -1), { binary: false }, done);
  } });
  // Errors can arrive while the upgrade is still pending, before NativeRpc attaches.
  output.on('error', () => undefined); input.on('error', () => undefined);
  socket.on('message', (data, binary) => {
    if (binary) { output.destroy(new IvyError('native_transport_invalid', 'Native proxy sent a binary protocol message.')); socket.terminate(); return; }
    const bytes = Array.isArray(data) ? Buffer.concat(data) : Buffer.from(data as Buffer);
    if (!output.write(Buffer.concat([bytes, Buffer.from('\n')]))) socket.pause();
  });
  output.on('drain', () => socket.resume());
  socket.on('error', error => { output.destroy(error); input.destroy(error); });
  socket.on('close', () => { output.end(); input.destroy(); });
  try {
    await new Promise<void>((resolve, reject) => { socket.once('open', resolve); socket.once('error', reject);
      socket.once('close', () => reject(new IvyError('native_transport_unavailable', 'Native proxy closed before WebSocket initialization.'))); });
  } catch (error) { socket.terminate(); tunnel.destroy(); throw error; }
  let closing: Promise<void> | null = null;
  const close = (): Promise<void> => {
    if (!closing) closing = (async () => {
      // A terminated tunnel leaves the shared app-server waiting for its stale
      // proxy lease before a replacement AgentManager can initialize. Complete
      // the WebSocket close handshake first; retain a short hard bound for a
      // broken peer so shutdown itself never becomes unbounded.
      if (socket.readyState === WebSocket.OPEN) socket.close(1000, 'ivy_agent_stopping');
      if (socket.readyState !== WebSocket.CLOSED) {
        const closed = new Promise<void>(resolve => socket.once('close', () => resolve()));
        const graceful = await Promise.race([closed.then(() => true), delay(1000).then(() => false)]);
        if (!graceful) { socket.terminate(); await Promise.race([closed, delay(250)]); }
      }
      tunnel.destroy(); output.end(); input.destroy();
    })();
    return closing;
  };
  return { input, output, close };
}
