// Protocol-only child; never loads a provider or native account.
import { createInterface } from 'node:readline';
import { connect } from 'node:net';
import { writeFileSync } from 'node:fs';
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
    if (mode === 'silent') return;
    if (request.method === 'initialized') {
      writeFileSync(marker + '.initialized', 'yes');
      if (mode === 'exit') setTimeout(() => process.exit(7), 30);
      return;
    }
    const result = request.method === 'initialize'
      ? { codexHome: home, userAgent: mode === 'wrong-version' ? 'codex/0.153.0' : 'codex/0.154.0' }
      : { data: [], nextCursor: null };
    process.stdout.write(JSON.stringify({ id: request.id, result }) + '\n');
  });
}
