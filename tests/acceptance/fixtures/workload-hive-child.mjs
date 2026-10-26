// Own a native Hive process. IPC controls this fixture; all service traffic uses Hive's public transport.
import assert from 'node:assert/strict';
import { copyFile, readFile, readdir } from 'node:fs/promises';
import { createReadStream } from 'node:fs';
import { dirname, join } from 'node:path';
import { createHash, randomUUID } from 'node:crypto';
import { setTimeout as delay } from 'node:timers/promises';
import { startNativeService } from '../../../tools/testing/native-service.mjs';

const options = JSON.parse(await readFile(process.argv[2], 'utf8'));
const dataRoot = dirname(options.filename);
const backupDirectory = join(dirname(dataRoot), 'backups', randomUUID());
const owner = await startNativeService({ schemaVersion: 1, componentId: 'hive', instanceId: 'workload-hive',
  serviceNodeId: 'workload.hive', hostId: 'workload', dataRoot, publicBaseUrl: options.publicBaseUrl,
  settings: { listenHost: '127.0.0.1', listenPort: options.listenPort, credentials: options.credentials,
    backup: { enabled: !!options.captureOnlineBackup, directory: backupDirectory, intervalHours: 24, retain: 2 } } },
  { artifact: options.artifactRoot, parentPid: process.pid });
let chain = Promise.resolve();
process.on('message', message => {
  chain = chain.then(async () => {
    try {
      let result;
      if (message.action === 'samples') result = { nativePid: owner.pid, coordinatorMemory: process.memoryUsage() };
      else if (message.action === 'backup') {
        assert.ok(options.captureOnlineBackup, 'This launch must own a scheduled online snapshot.');
        // A fresh backup directory makes the production scheduler take an online
        // snapshot after recovery. Only its completed publication is consumed;
        // no second connection opens the live, exclusively owned database.
        const deadline = Date.now() + 600000;
        let filename;
        while (!filename) {
          filename = (await readdir(backupDirectory)).find(name => /^hive-.*\.sqlite\.json$/.test(name));
          if (!filename) {
            if (owner.child.exitCode !== null || Date.now() >= deadline) throw Error('Native online snapshot did not complete: ' + owner.logs);
            await delay(50);
          }
        }
        const publication = JSON.parse(await readFile(join(backupDirectory, filename), 'utf8'));
        assert.equal(publication.buildId, owner.config.buildId);
        await copyFile(join(backupDirectory, filename.slice(0, -5)), message.destination, 1);
        const hash = createHash('sha256');
        for await (const bytes of createReadStream(message.destination)) hash.update(bytes);
        const bytesHash = 'sha256:' + hash.digest('hex');
        assert.equal(bytesHash, publication.bytesHash);
        result = { bytesHash, pages: publication.pages, createdAt: publication.createdAt,
          mechanism: 'Native Hive scheduled online backup after recovery' };
      } else if (message.action === 'close' || message.action === 'crash') {
        const exit = await (message.action === 'close' ? owner.close() : owner.kill());
        result = { nativePid: owner.pid, exit };
      } else throw Error('Unknown acceptance control.');
      process.send({ id: message.id, result }, () => { if (message.action === 'close' || message.action === 'crash') process.disconnect(); });
    } catch (error) { process.send({ id: message.id, error: { code: error.code, message: error.message } }); }
  });
});
process.on('disconnect', () => { void owner.close().finally(() => process.exit()); });
process.send({ ready: true, pid: owner.pid });
