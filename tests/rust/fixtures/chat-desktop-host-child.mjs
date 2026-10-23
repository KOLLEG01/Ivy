import fs from 'node:fs';
import {spawn} from 'node:child_process';
const [receipt, mode, release, ...args] = process.argv.slice(2);
if (mode === 'exit') process.exit(7);
if (mode === 'probe') {
  const devices = [0, 1, 2].map(fd => {
    const stat = fs.fstatSync(fd);
    return {character: stat.isCharacterDevice(), pipe: stat.isFIFO()};
  });
  fs.writeFileSync(receipt, JSON.stringify({
    cwd: process.cwd(), args, devices,
    stdinBytes: fs.readFileSync(0).length,
  }));
} else if (mode === 'tree') {
  const descendant = spawn(process.execPath, ['-e', 'setInterval(()=>{},1000)'], {
    stdio: 'ignore', windowsHide: true, shell: false,
  });
  descendant.once('error', error => { throw error; });
  descendant.once('spawn', () => {
    fs.writeFileSync(receipt, JSON.stringify({pid: process.pid, descendant: descendant.pid}));
  });
  setInterval(() => {
    if (fs.existsSync(release)) process.exit(0);
  }, 10);
} else {
  throw new Error('Unknown isolated process fixture mode.');
}
