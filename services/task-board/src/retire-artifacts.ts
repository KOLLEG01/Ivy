import { parseArgs } from 'node:util';
import { readFile } from 'node:fs/promises';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { IvyError, ServiceClient, requireThat, validateTaskBoard } from '../../../packages/sdk/src/node.js';
import type { TaskBoard } from '../../../packages/sdk/src/node.js';
import type { ServiceConnection } from '../../../packages/sdk/src/service.js';
import { atomicJson, instanceConfig } from '../../../packages/sdk/src/host.js';
import { taskBoardRegistry, taskBoardTools } from './runtime/registry.js';
import { TaskBoardStore } from './runtime/store.js';
import { TaskBoardEngine } from './runtime/engine.js';
import { retireTaskAttachments, type ArchivedAttachment, type ArtifactCleanupJob } from './runtime/artifact-retention.js';

interface Plan {
  schemaVersion: 1;
  hostId: string;
  serviceNodeId: string;
  scope: string;
  archiveManifest: ArtifactCleanupJob['archiveManifest'];
  taskIds: string[];
  files: (ArchivedAttachment & { ownerTaskId: string })[];
  referencePages: { objectId: string; revision: number; ownerTaskId: string }[];
}

/** Run only while the installed TaskBoard process is stopped. Active agent turns keep running at AgentManager. */
export async function retireArtifacts(configPath: string, planPath: string, stoppedPid: number): Promise<void> {
  requireThat(Number.isSafeInteger(stoppedPid) && stoppedPid > 0 && stoppedPid !== process.pid,
    'invalid_arguments', 'Name the stopped TaskBoard process.');
  try {
    process.kill(stoppedPid, 0);
    throw new Error('The TaskBoard process still exists; stop its owning instance before maintenance.');
  } catch (error) { if ((error as NodeJS.ErrnoException).code !== 'ESRCH') throw error; }
  const config = await instanceConfig(configPath);
  validateTaskBoard('Settings', config.settings);
  requireThat(config.componentId === 'task-board' && config.credential, 'invalid_arguments', 'Use the owning TaskBoard instance configuration.');
  const plan = JSON.parse(await readFile(planPath, 'utf8')) as Plan;
  requireThat(plan.schemaVersion === 1 && plan.hostId === config.hostId && plan.serviceNodeId === config.serviceNodeId &&
    /^sha256:[a-f0-9]{64}$/.test(plan.archiveManifest.contentHash) && Array.isArray(plan.files) && Array.isArray(plan.taskIds),
  'task_board_scope_mismatch', 'The verified export plan must name this owning host and service.');
  const settings = config.settings as unknown as TaskBoard.Settings;
  let connected!: (connection: ServiceConnection) => void;
  const connectionReady = new Promise<ServiceConnection>(resolve => { connected = resolve; });
  const client = new ServiceClient({ publicBaseUrl: config.publicBaseUrl, credential: () => config.credential!,
    identity: { serviceNodeId: config.serviceNodeId, serviceName: 'task-board', instanceMode: 'singleton',
      hostId: config.hostId, version: config.version, buildId: config.buildId, hiveProtocol: 1 },
    registry: taskBoardRegistry, handlers: Object.fromEntries(taskBoardTools.map(tool => ['task-board.' + tool.name,
      async () => { throw new IvyError('service_not_ready', 'TaskBoard attachment maintenance is in progress.'); }])), heartbeatMs: 2000,
    reconcile: async connection => { connected(connection); },
    readiness: async () => ({ ready: false, diagnostics: [] }),
    onState: state => { if (state.code) console.error(JSON.stringify({ phase: 'maintenance-connection', state: state.status, code: state.code })); },
  });
  client.start();
  let store: TaskBoardStore | undefined;
  const deadline = setTimeout(() => { void client.stop(); }, 30_000);
  try {
    const connection = await Promise.race([connectionReady, new Promise<never>((_resolve, reject) => {
      const timer = setTimeout(() => reject(new Error('Maintenance owner connection timed out.')), 30_000);
      timer.unref();
    })]);
    clearTimeout(deadline);
    store = new TaskBoardStore(connection, settings.rootObjectId, config.dataRoot);
    await new TaskBoardEngine(store, settings, { serviceNodeId: config.serviceNodeId, generation: connection.generation }).verifyOwner();
    const statePath = join(config.dataRoot, 'artifact-cleanup.json');
    const existingText = await readFile(statePath, 'utf8').catch(error => {
      if ((error as NodeJS.ErrnoException).code === 'ENOENT') return null; throw error;
    });
    const job: ArtifactCleanupJob = existingText ? JSON.parse(existingText) as ArtifactCleanupJob : {
      schemaVersion: 1, scope: plan.scope, archiveManifest: plan.archiveManifest, taskIds: [], referencePages: [], remaining: [],
      deletedFiles: 0, deletedBytes: 0, prunedRevisions: 0, updatedAt: new Date().toISOString() };
    requireThat(job.scope === plan.scope && job.archiveManifest.contentHash === plan.archiveManifest.contentHash,
      'mutation_conflict', 'An unfinished cleanup already names a different verified export.');
    await atomicJson(join(config.dataRoot, 'artifact-cleanup-plan.json'), plan);
    const skipped: string[] = [];
    let removed = 0;
    for (const taskId of plan.taskIds) {
      try {
        const result = await retireTaskAttachments(store, taskId, plan.files, plan.scope);
        removed += result.removed;
        if (!job.taskIds.includes(taskId)) job.taskIds.push(taskId);
        console.log(JSON.stringify({ taskId, retiredReferences: result.removed, revision: result.task.revision }));
      } catch (error) {
        if ((error as { code?: string }).code !== 'task_board_publication_busy') throw error;
        skipped.push(taskId);
      }
    }
    const pending = new Map(job.remaining.map(file => [file.object.objectId, file]));
    // Existing task IDs have already contributed their queue; do not re-add deleted files on a replay.
    const previousTasks = new Set(existingText ? (JSON.parse(existingText) as ArtifactCleanupJob).taskIds : []);
    for (const file of plan.files) if (job.taskIds.includes(file.ownerTaskId) && !previousTasks.has(file.ownerTaskId))
      pending.set(file.object.objectId, file);
    job.remaining = [...pending.values()];
    const pages = new Map(job.referencePages.map(page => [page.objectId, page]));
    for (const page of plan.referencePages) if (job.taskIds.includes(page.ownerTaskId) && !previousTasks.has(page.ownerTaskId))
      pages.set(page.objectId, { objectId: page.objectId, revision: page.revision });
    job.referencePages = [...pages.values()];
    if (job.remaining.length) delete job.completedAt;
    job.updatedAt = new Date().toISOString();
    await atomicJson(statePath, job);
    console.log(JSON.stringify({ phase: 'retirement-complete', removed, skipped, pendingFiles: job.remaining.length, statePath }));
  } finally { clearTimeout(deadline); await client.stop(); store?.close(); }
}

if (process.argv[1] && fileURLToPath(import.meta.url) === process.argv[1]) {
  const args = parseArgs({ options: { config: { type: 'string' }, plan: { type: 'string' }, 'stopped-pid': { type: 'string' } } }).values;
  if (!args.config || !args.plan) throw new Error('Use --config ABSOLUTE_PATH --plan ABSOLUTE_PATH --stopped-pid PID.');
  await retireArtifacts(args.config, args.plan, Number(args['stopped-pid']));
}
