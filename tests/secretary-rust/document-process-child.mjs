// Private finite faults for the Rust document process adapter, never a parser.
import {spawn} from 'node:child_process';
import {once} from 'node:events';
import {writeFileSync} from 'node:fs';

const hold = () => setInterval(() => {}, 1000);
function allocate(maximum) {
  const held = [];
  for (let size = 0; size < maximum; size += 16 * 1024 * 1024) {
    held.push(Buffer.alloc(16 * 1024 * 1024, 1));
  }
  globalThis.heldDocumentFixture = held;
}
if (process.argv[2] === '--member') {
  allocate(160 * 1024 * 1024); process.send?.('ready'); hold();
} else if (process.argv[2] === '--hold') {
  hold();
} else {
  const chunks = []; for await (const chunk of process.stdin) chunks.push(chunk);
  const request = JSON.parse(Buffer.concat(chunks).toString('utf8'));
  if (request.ready) writeFileSync(request.ready, String(process.pid), {flag: 'wx'});
  switch (request.mode) {
    case 'memory-single': allocate(1024 * 1024 * 1024); hold(); break;
    case 'memory-aggregate': {
      const children = Array.from({length: 2}, () => spawn(process.execPath, [process.argv[1], '--member'],
        {stdio: ['ignore', 'inherit', 'inherit', 'ipc'], windowsHide: true}));
      await Promise.all(children.map(child => once(child, 'message')));
      allocate(512 * 1024 * 1024); hold(); break;
    }
    case 'hang': hold(); break;
    case 'invalid': process.stdout.end('not JSON'); break;
    case 'exit': process.stderr.end('private OOM document contents'); process.exitCode = 27; break;
    case 'stdout-overflow': process.stdout.end('x'.repeat(9 * 1024 * 1024)); break;
    case 'stderr-overflow': process.stderr.end('x'.repeat(9 * 1024 * 1024)); process.stdout.end('{}'); break;
    case 'descendant':
      spawn(process.execPath, [process.argv[1], '--hold'], {stdio: ['ignore', 'inherit', 'inherit'], windowsHide: true}).unref();
      process.stdout.end('{"code":"media_document_no_text"}'); break;
    default: process.stdout.end('{"code":"media_document_no_text"}');
  }
}
