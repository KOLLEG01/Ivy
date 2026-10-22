import { PassThrough, Writable } from 'node:stream';
import type { Readable } from 'node:stream';
/** The official proxy is a byte tunnel. Its control socket carries WebSocket, not JSONL. */
export declare function proxyTransport(stdin: Writable, stdout: Readable): Promise<{
    input: Writable;
    output: PassThrough;
    close: () => Promise<void>;
}>;
