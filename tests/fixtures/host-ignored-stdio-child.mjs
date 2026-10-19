import {spawn} from 'node:child_process';
import {existsSync, fstatSync, readFileSync, writeFileSync} from 'node:fs';
import {fileURLToPath} from 'node:url';
const [receipt, mode, ...args] = process.argv.slice(2);
if (mode === 'descendant') {
  setTimeout(() => process.exit(92), 15000);
} else if (mode === 'tree') {
  const child = spawn(process.execPath, [fileURLToPath(import.meta.url), receipt, 'descendant'],
    {stdio: 'ignore', windowsHide: true, shell: false});
  child.once('spawn', () => writeFileSync(receipt, JSON.stringify({pid: process.pid, descendant: child.pid})));
  const timer = setInterval(() => { if (existsSync(receipt + '.release')) process.exit(0); }, 10);
  setTimeout(() => { clearInterval(timer); process.exit(91); }, 15000).unref();
} else {
  const stdin = readFileSync(0);
  const device = fd => ({character: fstatSync(fd).isCharacterDevice(), pipe: fstatSync(fd).isFIFO()});
  writeFileSync(receipt, JSON.stringify({args, argv0: process.argv0, cwd: process.cwd(),
    inherited: process.env.IVY_HOST_IGNORED_INHERIT ?? null, warnings: process.env.NODE_NO_WARNINGS ?? null,
    stdinBytes: stdin.length, devices: [device(0), device(1), device(2)]}));
  process.stdout.write('ignored stdout\n');
  process.stderr.write('ignored stderr\n');
  process.exitCode = mode === 'exit' ? 7 : 0;
}
