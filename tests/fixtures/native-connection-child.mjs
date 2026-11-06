// Protocol-only child; never loads a provider or native account.
import { createInterface } from 'node:readline';
import { connect } from 'node:net';
import { appendFileSync, writeFileSync, existsSync } from 'node:fs';
const [mode, home, marker] = process.argv.slice(2);
writeFileSync(marker, String(process.pid));
// Raw stderr must be drained independently of protocol output.
process.stderr.write('x'.repeat(128 * 1024));
if (mode === 'proxy') {
  const socket = connect(Number(home), '127.0.0.1');
  process.stdin.pipe(socket);
  socket.pipe(process.stdout);
} else {
  createInterface({ input: process.stdin }).on('line', line => {
    const request = JSON.parse(line);
    if (mode.startsWith('launch-')) appendFileSync(marker + '.frames', line + '\n');
    if (mode === 'silent') return;
    if (request.method === 'initialized') {
      writeFileSync(marker + '.initialized', 'yes');
      if (mode === 'exit') setTimeout(() => process.exit(7), 30);
      if (mode === 'launch-duplex') {
        process.stdout.write(JSON.stringify({ id: 7, method: 'currentTime/read', params: { threadId: 'voice-thread' } }) + '\n');
        process.stdout.write(JSON.stringify({ method: 'serverRequest/resolved', params: { requestId: 7, threadId: 'voice-thread' } }) + '\n');
      }
      return;
    }
    if (mode.startsWith('launch-') && !request.method) return;
    const result = request.method === 'initialize'
      ? { codexHome: home, userAgent: ['wrong-version', 'launch-wrong-version'].includes(mode) ? 'codex/0.153.0' : 'codex/0.154.0' }
      : { data: [], nextCursor: null };
    const reply = () => process.stdout.write(JSON.stringify({ id: request.id, result }) + '\n');
    if (mode === 'launch-host' && request.method === 'model/list' && existsSync(marker + '.hold')) {
      writeFileSync(marker + '.pending', 'yes');
      const timer = setInterval(() => {
        if (!existsSync(marker + '.hold')) { clearInterval(timer); reply(); }
      }, 10);
    } else reply();
  });
}
