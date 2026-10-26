// A compiled executable plus a stdio/TCP peer exercises the production launch owner.
// The peer speaks only the external provider protocol and cannot run model work.
import { after } from 'node:test';
import { spawnSync } from 'node:child_process';
import { createServer } from 'node:net';
import { once } from 'node:events';
import { createInterface } from 'node:readline';
import { createHash } from 'node:crypto';
import { copyFile, mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { delimiter, dirname, join, resolve } from 'node:path';

let compiled;
after(async () => {
  if (compiled) await rm(dirname(await compiled), { recursive: true, force: true, maxRetries: 3 });
});
async function compile() {
  const directory = await mkdtemp(join(tmpdir(), 'ivy-synthetic-native-'));
  const executable = join(directory, 'native' + (process.platform === 'win32' ? '.exe' : ''));
  const result = spawnSync(process.env.IVY_TEST_RUSTC ?? process.env.RUSTC ?? 'rustc',
    ['--edition=2024', '-C', 'debuginfo=0', resolve('tests/fixtures/native-launch-child.rs'), '-o', executable],
    { encoding: 'utf8', windowsHide: true, timeout: 60000 });
  if (result.error || result.status !== 0) throw result.error ?? Error(result.stderr);
  return executable;
}

export async function syntheticNative(root, version, onConnection) {
  const bin = join(root, 'bin'); await mkdir(bin, { recursive: true });
  const executable = join(bin, 'native' + (process.platform === 'win32' ? '.exe' : ''));
  await copyFile(await (compiled ??= compile()), executable);
  const hash = 'sha256:' + createHash('sha256').update(await readFile(executable)).digest('hex');
  const shell = join(bin, 'pwsh.exe');
  if (process.platform === 'win32') await copyFile(executable, shell);
  const sockets = new Set();
  const server = createServer(socket => {
    sockets.add(socket); socket.on('close', () => sockets.delete(socket));
    socket.on('error', () => socket.destroy());
    const receive = onConnection(frame => socket.write(JSON.stringify(frame) + '\n'));
    const reader = createInterface({ input: socket });
    reader.on('error', () => reader.close());
    reader.on('line', line => receive(JSON.parse(line)));
  });
  server.listen(0, '127.0.0.1'); await once(server, 'listening');
  await writeFile(join(bin, 'launch-fixture.txt'), [process.execPath, resolve('tests/fixtures/native-connection-child.mjs'),
    'proxy', String(server.address().port), join(root, 'native-child'), '{}', version, ''].join('\n'));
  return {
    settings: { nativeExecutable: executable, nativeExecutableHash: hash, nativeVersion: version,
      appServer: { mode: 'owned-stdio' }, ...(process.platform === 'win32' ? { windowsShell: { executable: shell, executableHash: hash } } : {}) },
    env: { PATH: bin + delimiter + process.env.PATH },
    close: async () => { for (const socket of sockets) socket.destroy(); await new Promise(resolve => server.close(resolve)); },
  };
}
