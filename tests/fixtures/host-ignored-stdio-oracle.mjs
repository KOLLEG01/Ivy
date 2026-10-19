import {spawn} from 'node:child_process';
import {readFileSync} from 'node:fs';
const input = JSON.parse(readFileSync(0, 'utf8'));
let settled = false;
const finish = value => {
  if (settled) return;
  settled = true;
  process.stdout.write(JSON.stringify(value));
};
try {
  const options = {windowsHide: true, stdio: 'ignore', shell: false};
  if (Object.hasOwn(input, 'cwd')) options.cwd = input.cwd;
  if (Object.hasOwn(input, 'env')) options.env = input.env;
  const child = spawn(input.file, input.args, options);
  const ignored = child.stdin === null && child.stdout === null && child.stderr === null;
  const timer = setTimeout(() => { child.kill(); finish({kind: 'timeout'}); }, 5000);
  child.once('error', error => { clearTimeout(timer); finish({kind: 'spawn', code: error.code}); });
  child.once('exit', (code, signal) => { clearTimeout(timer); finish({kind: 'exit', code, signal, ignored}); });
} catch (error) { finish({kind: 'spawn', code: error.code}); }
