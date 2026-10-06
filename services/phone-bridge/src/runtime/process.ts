import { open, realpath, stat } from 'node:fs/promises';
import { isAbsolute, join, resolve } from 'node:path';
import { fileHash } from '../../../../packages/host-runtime/src/artifact.js';
import { inside, jsonFile } from '../../../../packages/host-runtime/src/config.js';
import { startProcess } from '../../../../packages/host-runtime/src/process.js';
import { clearProcessStopFence, processStopFence, recordProcessStopFence, requireClearProcessStopFence } from '../../../../packages/host-runtime/src/process-fence.js';
import { IvyError, requireThat } from '../../../../packages/sdk/src/node.js';
import { PhoneNativeClient, newPhoneEpoch } from './native.js';
import type { PhoneJournal } from './journal.js';

export interface PhoneProcessOptions {
  artifactRoot: string; executable: string; executableHash: string;
  journal: PhoneJournal;
}

type LeaseProbe = (path: string) => Promise<boolean>;
const nativeLeaseReleased: LeaseProbe = async path => {
  let lease;
  try { lease = await open(path, 'r+'); }
  catch (error) {
    if (['EBUSY', 'EACCES', 'EPERM'].includes((error as NodeJS.ErrnoException).code ?? '')) return false;
    throw error;
  }
  await lease.close(); return true;
};

/** Clear only a matching native fence after its lifetime lock is exclusively available. */
export async function reconcilePhoneProcessStopFence(dataRoot: string, probe: LeaseProbe = nativeLeaseReleased): Promise<boolean> {
  const fence = await processStopFence(dataRoot); if (!fence || fence.owner !== 'native') return false;
  const owner = await jsonFile<Record<string, unknown>>(join(dataRoot, 'phone-native-owner.json'), 4096);
  requireThat(Object.keys(owner).sort().join(',') === 'creationFileTime,epoch,pid,schemaVersion' && owner['schemaVersion'] === 1 && owner['epoch'] === fence.ownerId &&
    Number.isSafeInteger(owner['pid']) && Number(owner['pid']) > 0 && typeof owner['creationFileTime'] === 'string' && /^[1-9][0-9]{0,19}$/.test(owner['creationFileTime']),
  'outcome_unknown', 'Phone native owner record does not match its retained stop fence.');
  if (!await probe(join(dataRoot, 'phone-native.lock'))) return false;
  return clearProcessStopFence(dataRoot, fence);
}

/** Uses the existing Windows job launcher, artifact verification and durable unknown-stop fence. */
export async function startPhoneProcess(options: PhoneProcessOptions) {
  requireThat(process.platform === 'win32' && process.arch === 'x64', 'phone_platform', 'Phone requires its owning Windows x64 host.');
  const { journal } = options, dataRoot = journal.dataRoot;
  requireThat(isAbsolute(options.artifactRoot) && !isAbsolute(options.executable), 'invalid_arguments', 'Phone executable must be relative to its owned artifact.');
  await reconcilePhoneProcessStopFence(dataRoot);
  await requireClearProcessStopFence(dataRoot);
  const root = await realpath(options.artifactRoot), executable = await realpath(resolve(root, options.executable));
  requireThat(inside(root, executable) && (await stat(executable)).isFile(), 'target_conflict', 'Phone executable leaves its verified artifact.');
  requireThat(await fileHash(executable) === options.executableHash, 'phone_build_mismatch', 'Phone executable differs from its selected artifact.');
  const epoch = newPhoneEpoch(), previous = journal.epoch;
  const childProcess = await startProcess({ executable: options.executable, args: ['--epoch', epoch, '--owner-root', dataRoot], timeoutMs: 15000 }, root, {},
    { captureOutput: false, jobLauncher: join(root, 'dist/native/ivy-job.exe') });
  let client: PhoneNativeClient | null = null;
  const completion = childProcess.completion.then(async result => {
    if (result.errorCode === 'outcome_unknown') await recordProcessStopFence(dataRoot, { schemaVersion: 1, owner: 'native', ownerId: epoch,
      processId: childProcess.child.pid ?? null, observedAt: new Date().toISOString(), code: 'outcome_unknown' });
    const code = result.errorCode ?? 'phone_process_exited'; client?.close(code); journal.loseEpoch(epoch); return result;
  });
  let stopping: Promise<void> | null = null;
  const close = (): Promise<void> => {
    if (stopping) return stopping;
    stopping = (async () => {
      client?.close(); childProcess.child.stdin?.end();
      let timer: NodeJS.Timeout | null = null;
      try {
        await Promise.race([completion, new Promise<void>(resolve => { timer = setTimeout(resolve, 5000); })]);
      } catch { /* Always stop the owned process even if evidence storage fails. */ }
      finally { if (timer) clearTimeout(timer); }
      const result = await childProcess.stop(250); await completion;
      if (result.errorCode === 'outcome_unknown') throw new IvyError('outcome_unknown', 'Phone stop remains unknown; its durable fence prevents another owner.', 'unknown');
    })(); return stopping;
  };
  try {
    client = new PhoneNativeClient(epoch, childProcess.child.stdin!, childProcess.child.stdout!, journal.hooks);
    void client.closed.then(close).catch(() => undefined);
    void completion.catch(() => client?.close('phone_stop_evidence_failed'));
    // Loading the self-contained CLR and acquiring the owner lease can exceed the live 1s
    // observation deadline on a cold machine. No effect is admitted during this bounded hello.
    // Native's 15s lease starts only after its pipe server exists; subsequent heartbeats retain
    // their ordinary strict deadline.
    const ready = await client.request('heartbeat', {}, null, 15000);
    requireThat(ready.ok, 'phone_start_failed', 'Original native Phone owner did not complete its startup handshake.');
    client.startHeartbeat();
    const hello = await client.observe();
    requireThat(!hello.configured && hello.call === null && hello.endpoint === null && hello.registration === null,
      'phone_start_failed', 'Phone native process must begin unconfigured behind its exclusive owner lease.');
    requireThat(journal.epoch === previous, 'phone_epoch_mismatch', 'Phone journal ownership changed during launch.');
    // Successful original status is possible only after the verified native entrypoint acquired
    // its lifetime lease and proved the recorded predecessor exited. No effect client escapes
    // before old unknown intents are retained and the replacement epoch is durable.
    if (previous) journal.loseEpoch(previous);
    journal.beginEpoch(epoch);
    return { client, epoch, completion, close };
  } catch (error) { await close(); throw error; }
}
