import { spawn, spawnSync } from 'node:child_process';
import { randomUUID } from 'node:crypto';
import { copyFile, mkdir, readFile, rename, symlink, writeFile } from 'node:fs/promises';
import { createServer } from 'node:net';
import { dirname, join, resolve } from 'node:path';
import { setTimeout as delay } from 'node:timers/promises';

export function artifactRoot(component, supplied) {
  return resolve(supplied ?? process.env['IVY_TEST_' + component.replaceAll('-', '_').toUpperCase() + '_ARTIFACT'] ?? '.');
}
export function nativeExecutable(artifact, name) {
  return join(artifact, 'dist/native', process.platform + '-' + process.arch, name + (process.platform === 'win32' ? '.exe' : ''));
}
const json = async path => JSON.parse(await readFile(path, 'utf8'));
export async function writeJson(path, value) {
  await mkdir(dirname(path), { recursive: true });
  const temporary = path + '.' + randomUUID() + '.tmp';
  await writeFile(temporary, JSON.stringify(value) + '\n');
  await rename(temporary, path);
}

/** Own a real native service through its public instance/health/control format. */
export async function startNativeService(configuration, { artifact, env, ready = true, parentPid, executable: selectedExecutable } = {}) {
  artifact = artifactRoot(configuration.componentId, artifact);
  const identity = await json(join(artifact, 'dist/build-info.json'));
  const config = { ...configuration, artifactRoot: artifact, version: identity.version, buildId: identity.buildId };
  const path = join(dirname(config.dataRoot), config.instanceId + '.json');
  await writeJson(path, config);
  const launchId = randomUUID();
  const executable = selectedExecutable ?? nativeExecutable(artifact, 'ivy-' + config.componentId);
  const job = process.platform === 'win32' && parentPid !== undefined;
  const child = spawn(job ? nativeExecutable(artifact, 'ivy-job') : executable,
    [...(job ? ['--parent', String(parentPid), executable] : []), '--config', path], {
    windowsHide: true, stdio: ['ignore', 'pipe', 'pipe'], env: { ...process.env, ...env, IVY_LAUNCH_ID: launchId },
  });
  let logs = '';
  let nativePid, launchError;
  child.once('error', error => { launchError = error; });
  for (const stream of [child.stdout, child.stderr]) stream.on('data', bytes => { logs = (logs + bytes).slice(-64000); });
  const closed = new Promise((yes, no) => { child.once('error', no); child.once('close', (code, signal) => yes({ code, signal })); });
  // Register immediately: a missing executable must not become an unhandled rejection.
  closed.catch(() => {});
  const exited = () => child.exitCode !== null || child.signalCode !== null;
  const kill = async () => {
    if (!exited() && child.pid) {
      if (process.platform === 'win32') spawnSync('taskkill.exe', ['/PID', String(child.pid), '/T', '/F'], { windowsHide: true, stdio: 'ignore', timeout: 15000 });
      else child.kill('SIGKILL');
    }
    return closed;
  };
  const waitReady = async (timeout = 45000) => {
    const end = Date.now() + timeout;
    while (Date.now() < end) {
      if (launchError) throw launchError;
      if (exited()) throw Error(config.componentId + ' exited before readiness: ' + logs);
      const health = await json(join(config.dataRoot, 'health.json')).catch(() => null);
      if (health?.ready && (job ? Number.isSafeInteger(health.pid) && health.pid > 0 : health.pid === child.pid) && health.launchId === launchId) {
        nativePid = health.pid;
        return health;
      }
      await delay(50);
    }
    throw Error(config.componentId + ' readiness timed out: ' + logs);
  };
  const close = async () => {
    if (exited()) return closed;
    try {
      const health = await json(join(config.dataRoot, 'health.json'));
      if (health.pid !== (nativePid ?? child.pid) || health.launchId !== launchId) throw Error('Native service health owner changed.');
      await writeJson(join(config.dataRoot, 'control.json'), { schemaVersion: 1, instanceId: config.instanceId,
        launchId, bootId: health.bootId, action: 'shutdown', requestedAt: new Date().toISOString() });
      const result = await Promise.race([closed, delay(15000, null, { ref: false })]);
      if (!result || result.code !== 0 || result.signal !== null) throw Error(config.componentId + ' shutdown failed: ' + logs);
      return result;
    } finally { if (!exited()) await kill(); }
  };
  const owner = { config, path, child, closed, launchId, ready: waitReady, close, kill,
    get pid() { return nativePid ?? child.pid; }, get logs() { return logs; } };
  try { if (ready) await waitReady(); return owner; }
  catch (error) { await kill().catch(() => {}); throw error; }
}

export async function startTestHive({ root, artifact, credentials, port, settings = {}, consoleRoot, executable, env } = {}) {
  if (Object.values(settings).some(value => typeof value === 'function'))
    throw Error('Native Hive fixtures accept JSON settings; external transports need a protocol fixture.');
  if (!port) {
    const listener = createServer();
    await new Promise((yes, no) => { listener.once('error', no); listener.listen(0, '127.0.0.1', yes); });
    port = listener.address().port;
    await new Promise((yes, no) => listener.close(error => error ? no(error) : yes()));
  }
  if (consoleRoot) {
    const selected = artifactRoot('hive', artifact);
    artifact = join(root, 'hive-artifact');
    await mkdir(join(artifact, 'dist'), { recursive: true });
    await copyFile(join(selected, 'dist/build-info.json'), join(artifact, 'dist/build-info.json'));
    const link = async (target, name) => symlink(resolve(target), join(artifact, 'dist', name), process.platform === 'win32' ? 'junction' : 'dir');
    await link(join(selected, 'dist/native'), 'native');
    await link(consoleRoot, 'console');
  }
  return startNativeService({ schemaVersion: 1, componentId: 'hive', instanceId: 'hive', serviceNodeId: 'fixture.hive', hostId: 'fixture',
    dataRoot: join(root, 'hive-data'), publicBaseUrl: `http://127.0.0.1:${port}/ivy`,
    settings: { listenHost: '127.0.0.1', listenPort: port, credentials,
      backup: { enabled: false, directory: join(root, 'backups'), intervalHours: 24, retain: 2 }, ...settings } }, { artifact, executable, env });
}

/** Fault only the storage child of this owned Hive; keep its network owner alive. */
export async function crashStorageWorker(owner) {
  if (owner.config.componentId !== 'hive' || !Number.isSafeInteger(owner.pid) || owner.child.exitCode !== null)
    throw Error('A running owned Hive is required.');
  let workers = [];
  if (process.platform === 'win32') {
    const script = `$ErrorActionPreference='Stop'; $parents=@(${owner.pid}); $workers=@(); for($depth=0;$depth -lt 3;$depth++){ $children=@(foreach($parent in $parents){Get-CimInstance Win32_Process -Filter "ParentProcessId = $parent"}); $workers+=@($children | Where-Object {$_.Name -eq 'ivy-hive.exe' -and $_.CommandLine -match '(?:^|\\s)--storage-worker(?:\\s|$)'} | ForEach-Object {$_.ProcessId}); $parents=@($children | ForEach-Object {$_.ProcessId}); } ConvertTo-Json -Compress -InputObject @($workers)`;
    const result = spawnSync('powershell.exe', ['-NoProfile', '-NonInteractive', '-EncodedCommand', Buffer.from(script, 'utf16le').toString('base64')],
      { encoding: 'utf8', windowsHide: true, timeout: 15000 });
    if (result.error || result.status !== 0) throw result.error ?? Error(result.stderr);
    workers = JSON.parse(result.stdout);
  } else {
    let parents = [owner.pid];
    for (let depth = 0; depth < 3; depth++) {
      const children = [];
      for (const parent of parents) {
        const text = await readFile(`/proc/${parent}/task/${parent}/children`, 'utf8').catch(() => '');
        for (const pid of text.trim().split(/\s+/).filter(Boolean).map(Number)) {
          children.push(pid);
          const args = (await readFile(`/proc/${pid}/cmdline`, 'utf8').catch(() => '')).split('\0');
          if (args.includes('--storage-worker') && /(?:^|\/)ivy-hive$/.test(args[0])) workers.push(pid);
        }
      }
      parents = children;
    }
  }
  if (workers.length !== 1 || !Number.isSafeInteger(workers[0]) || workers[0] <= 0)
    throw Error('Expected exactly one storage worker under this Hive, found ' + JSON.stringify(workers));
  process.kill(workers[0], 'SIGKILL');
  return workers[0];
}
