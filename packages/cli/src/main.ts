import { preparationProgress } from '../../host-runtime/src/preparation-progress.js';
import { parseArgs } from 'node:util';
import { resolve, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { setTimeout as delay } from 'node:timers/promises';
import { hostConfig } from '../../host-runtime/src/host-config.js';
import { defaultHostConfigPath } from '../../host-runtime/src/layout.js';
import { freshExecutorStatus, HostJournal, requiresBootstrapReplacement, terminalPhases } from '../../host-runtime/src/journal.js';
import { captureSource } from '../../host-runtime/src/source.js';
import { compactPreparations, preparationInventory } from '../../host-runtime/src/preparations.js';
import { collectHostStorage } from '../../host-runtime/src/storage-retention.js';
import { backupHost } from '../../host-runtime/src/private-backup.js';
import { restoreHost } from '../../host-runtime/src/restore.js';
import { inspectBootstrap, updateBootstrap, bootstrapStatus } from '../../host-runtime/src/bootstrap-maintenance.js';
import { bootstrapPlan, installBootstrap } from '../../host-runtime/src/bootstrap.js';
import { runtimeReset, runtimeResetHost } from '../../host-runtime/src/runtime-reset.js';
import { jsonFile } from '../../host-runtime/src/config.js';
import { IvyError, requireThat } from '../../contracts/src/errors.js';
import { validateHost } from '../../contracts/src/host-validation.js';
import type { Host } from '../../contracts/src/generated.js';

export interface CliResult { exitCode: number; output: Host.CliOutput }
export async function cli(args: string[], environment: NodeJS.ProcessEnv = process.env, onProgress?: (message: string) => void): Promise<CliResult> {
  let journal: HostJournal | null = null, accepted: Host.JournalEntry | null = null;
  try {
    let parsed: ReturnType<typeof parseArgs>;
    try { parsed = parseArgs({ args, strict: true, allowPositionals: true, options: {
      config: { type: 'string' }, json: { type: 'boolean' }, component: { type: 'string' }, source: { type: 'string' }, instance: { type: 'string' },
      candidate: { type: 'string' }, 'operation-id': { type: 'string' }, deployment: { type: 'string' }, to: { type: 'string' }, 'wait-ms': { type: 'string' },
      destination: { type: 'string' }, backup: { type: 'string' }, 'backup-hash': { type: 'string' }, request: { type: 'string' },
      apply: { type: 'boolean' }, confirm: { type: 'string' }, phase: { type: 'string' }, 'reset-id': { type: 'string' }, 'runtime-epoch': { type: 'string' }, 'scope-roots': { type: 'string' }, 'publisher-digest': { type: 'string' },
    } }); } catch { throw new IvyError('invalid_arguments', 'Invalid command options. See the Ivy local CLI contract.'); }
    const values = parsed.values as Record<string, string | boolean | undefined>;
    requireThat(parsed.positionals.length === 1, 'invalid_arguments', 'Select exactly one Ivy command.');
    const command = parsed.positionals[0]!;
    const supported: Record<string, string[]> = { prepare: ['component', 'source'], deploy: ['instance', 'candidate', 'source', 'operation-id', 'wait-ms'], rollback: ['instance', 'to', 'operation-id', 'wait-ms'], status: ['deployment'], restart: ['instance', 'operation-id', 'wait-ms'], enable: ['instance', 'operation-id', 'wait-ms'], disable: ['instance', 'operation-id', 'wait-ms'] };
    supported['preparations'] = []; supported['compact-preparations'] = []; supported['collect-storage'] = [];
    supported['backup'] = ['destination']; supported['restore'] = ['backup', 'backup-hash'];
    supported['runtime-reset'] = ['apply', 'confirm'];
    supported['runtime-reset-host'] = ['phase', 'reset-id', 'runtime-epoch', 'scope-roots', 'publisher-digest'];
    supported['inspect-bootstrap'] = []; supported['update-bootstrap'] = ['request']; supported['bootstrap-status'] = ['operation-id']; supported['install-bootstrap'] = ['candidate'];
    requireThat(Object.hasOwn(supported, command), 'invalid_arguments', 'Unknown Ivy command.');
    requireThat(Object.keys(values).every(key => ['json', 'config', ...supported[command]!].includes(key)), 'invalid_arguments', 'This option does not belong to the selected command.');
    const waitMs = values['wait-ms'] === undefined ? 0 : Number(values['wait-ms']);
    requireThat(Number.isInteger(waitMs) && waitMs >= 0 && waitMs <= 600_000, 'invalid_arguments', '--wait-ms must be a finite integer from 0 to 600000.');
    const configPath = values['config'] ?? environment['IVY_HOST_CONFIG'] ?? defaultHostConfigPath(environment);
    requireThat(typeof configPath === 'string' && configPath.length > 0, 'invalid_arguments', 'Use --config PATH or IVY_HOST_CONFIG to select this host installation.');
    // Both source and compiled directory layouts resolve the owning distribution root explicitly.
    const modulePath = fileURLToPath(new URL('../../../', import.meta.url));
    const bootstrapRoot = modulePath.endsWith('dist\\') || modulePath.endsWith('dist/') ? resolve(modulePath, '..') : modulePath;
    const required = (key: string): string => { const value = values[key]; requireThat(typeof value === 'string' && value.length > 0, 'invalid_arguments', 'Missing --' + key + '.'); return value; };
    if (command === 'restore') {
      const output: Host.CliOutput = { schemaVersion: 1, ok: true, code: 'restore_completed',
        data: await restoreHost(resolve(required('backup')), required('backup-hash'), resolve(configPath), bootstrapRoot) };
      validateHost('CliOutput', output); return { exitCode: 0, output };
    }
    const config = await hostConfig(resolve(configPath)); journal = new HostJournal(config);
    if (command === 'runtime-reset' || command === 'runtime-reset-host') {
      journal.close(); journal = null;
      const data = command === 'runtime-reset'
        ? await runtimeReset(config, resolve(configPath), bootstrapRoot, values['apply'] === true, values['confirm'] as string | undefined)
        : await runtimeResetHost(config, resolve(configPath), bootstrapRoot, required('phase') as 'preview'|'prepare'|'clear'|'complete'|'bootstrap'|'release'|'confirm', values['reset-id'] as string | undefined, values['runtime-epoch'] as string | undefined, values['scope-roots'] as string | undefined, values['publisher-digest'] as string | undefined);
      const output: Host.CliOutput = { schemaVersion: 1, ok: true, code: command === 'runtime-reset' ? (values['apply'] ? 'runtime_reset_applied' : 'runtime_reset_preview') : 'runtime_reset_host_' + values['phase'], data: data as Host.CliOutput['data'] };
      validateHost('CliOutput', output); return { exitCode: 0, output };
    }
    const deploymentData = async (entry: Host.JournalEntry): Promise<Host.CliOutput['data']> => {
      const progress = await preparationProgress(config, entry);
      return (progress ? { ...entry, progress } : entry) as Host.CliOutput['data'];
    };
    let output: Host.CliOutput;
    if (command === 'install-bootstrap') {
      const plan = await bootstrapPlan(resolve(configPath), required('candidate'));
      output = await installBootstrap(plan, bootstrapRoot);
    } else if (command === 'inspect-bootstrap') {
      output = { schemaVersion: 1, ok: true, code: 'bootstrap_snapshot', data: await inspectBootstrap(resolve(configPath), bootstrapRoot) };
    } else if (command === 'update-bootstrap' || command === 'bootstrap-status') {
      const record = command === 'update-bootstrap'
        ? await updateBootstrap(resolve(configPath), await jsonFile<Host.BootstrapMaintenanceRequest>(resolve(required('request'))), bootstrapRoot)
        : await bootstrapStatus(config, required('operation-id'));
      output = { schemaVersion: 1, ok: record.phase !== 'needs_attention', code: 'bootstrap_' + record.phase, data: record };
    } else if (command === 'backup') {
      output = { schemaVersion: 1, ok: true, code: 'backup_completed', data: await backupHost(resolve(configPath), resolve(required('destination')), bootstrapRoot) };
    } else if (command === 'preparations' || command === 'compact-preparations') {
      output = { schemaVersion: 1, ok: true, code: command === 'preparations' ? 'preparation_inventory' : 'preparation_compaction',
        data: command === 'preparations' ? await preparationInventory(config) : await compactPreparations(config) };
    } else if (command === 'collect-storage') {
      const removed = await collectHostStorage(config);
      const status = journal.storageRetentionStatus()!;
      output = { schemaVersion: 1, ok: status.state === 'succeeded', code: 'storage_retention_' + status.state,
        data: { removed, status } as Host.CliOutput['data'] };
    } else if (command === 'prepare') {
      const componentId = required('component');
      const snapshot = await captureSource(resolve(required('source')), config);
      const { prepareCandidate } = await import('../../host-runtime/src/prepare.js');
      const candidate = await prepareCandidate(snapshot, componentId, config, bootstrapRoot);
      output = { schemaVersion: 1, ok: true, code: 'candidate_prepared', data: candidate };
    } else if (command === 'status') {
      if (values['deployment']) {
        const entry = journal.get(required('deployment'));
        const failed = ['failed', 'needs_attention', 'rolled_back', 'rolling_back'].includes(entry.record.phase);
        output = { schemaVersion: 1, ok: !failed, code: 'deployment_' + entry.record.phase, deploymentId: entry.record.deploymentId, data: await deploymentData(entry) };
      } else {
        const executor = await jsonFile<Host.ExecutorStatus>(join(config.runtimeRoot, 'executor.json')).then(value => {
          try { validateHost('ExecutorStatus', value); return value.hostId === config.hostId && freshExecutorStatus(value) ? value : null; } catch { return null; }
        }).catch(() => null);
        const storageRetention = journal.storageRetentionStatus();
        output = { schemaVersion: 1, ok: true, code: 'host_status', data: { hostId: config.hostId, executor,
          ...(storageRetention ? { storageRetention } : {}),
          instances: config.instances.map(instance => ({ instanceId: instance.instanceId, componentId: instance.componentId, enabled: journal!.installed(instance.instanceId)?.enabled ?? instance.enabled, installed: journal!.installed(instance.instanceId) })),
          observations: journal.currentObservations(executor), unfinished: journal.unfinished().map(entry => entry.record), restoredFrom: config.restoredFrom ?? null } as Host.CliOutput['data'] };
      }
    } else {
      const request: Host.LocalRequest = { action: command as Host.LocalRequest['action'], instanceId: required('instance'), operationId: required('operation-id'),
        ...(values['candidate'] ? { candidateId: required('candidate') } : {}), ...(values['source'] ? { source: resolve(required('source')) } : {}), ...(values['to'] ? { targetBuild: required('to') } : {}) };
      requireThat(!requiresBootstrapReplacement(journal.instance(request.instanceId)) || !journal.installed(request.instanceId), 'restart_required',
        'Bootstrap-owned releases use prepare followed by install-bootstrap; normal deployment cannot observe their OS-owned drain.');
      const prior = journal.operation(request.operationId);
      if (prior) accepted = journal.accept(request);
      else if (request.source) {
        requireThat(!request.candidateId, 'invalid_arguments', 'Deploy requires exactly one candidate or source.');
        journal.instance(request.instanceId);
        const snapshot = await captureSource(request.source, config); accepted = journal.accept(request, snapshot);
      } else accepted = journal.accept(request);
      const end = Date.now() + waitMs;
      let nextProgress = 0, lastMessage = '', lastReported = 0, lastFailure = '';
      while (!terminalPhases.has(accepted.record.phase) && Date.now() < end) {
        if (onProgress && accepted.record.errorCode && accepted.record.errorCode !== lastFailure) {
          lastFailure = accepted.record.errorCode;
          try { onProgress(`${accepted.record.deploymentId}: FAILED ${lastFailure} | ${accepted.record.readiness.message} | ${accepted.record.phase}`); } catch { /* Display only. */ }
        }
        if (onProgress && Date.now() >= nextProgress) {
          const progress = await preparationProgress(config, accepted);
          const step = progress?.step ?? accepted.record.phase;
          if (step !== lastMessage || Date.now() - lastReported >= 15000) {
            const seconds = Math.round((progress?.stepElapsedMs ?? Date.now() - Date.parse(accepted.record.updatedAt)) / 1000);
            const message = `${accepted.record.deploymentId}: ${step} (${seconds}s)` +
              (progress?.command ? ` | ${progress.command}` : '') +
              (progress?.fileBytes != null ? ` | ${progress.fileBytes} bytes` : '') +
              (progress?.lastOutputAt ? ` | last output ${progress.lastOutputAt}` : '') +
              (progress && progress.heartbeatAgeMs > 15000 ? ` | heartbeat ${Math.round(progress.heartbeatAgeMs / 1000)}s old` : '');
            try { onProgress(message); } catch { /* A display callback cannot change the operation. */ }
            lastMessage = step; lastReported = Date.now();
          }
          nextProgress = Date.now() + 5000;
        }
        await delay(Math.max(0, Math.min(250, end - Date.now()))); accepted = journal.get(accepted.record.deploymentId);
      }
      const failed = ['failed', 'needs_attention', 'rolled_back', 'rolling_back'].includes(accepted.record.phase);
      output = { schemaVersion: 1, ok: !failed, code: 'deployment_' + accepted.record.phase, deploymentId: accepted.record.deploymentId, data: await deploymentData(accepted) };
    }
    validateHost('CliOutput', output);
    return { exitCode: ['deployment_needs_attention', 'bootstrap_needs_attention'].includes(output.code) ? 3 : output.ok ? 0 : 1, output };
  } catch (error) {
    const failure = error instanceof SyntaxError ? new IvyError('invalid_arguments', 'Configuration contains malformed JSON.') : IvyError.from(error);
    const output: Host.CliOutput = { schemaVersion: 1, ok: false, code: failure.code, ...(accepted ? { deploymentId: accepted.record.deploymentId } : {}),
      data: { message: failure.message, outcome: failure.outcome, ...(failure.details === undefined ? {} : { details: failure.details }) } as Host.CliOutput['data'] };
    return { exitCode: failure.code === 'needs_attention' ? 3 : failure.code === 'invalid_arguments' ? 2 : 1, output };
  } finally { journal?.close(); }
}

if (process.argv[1] && fileURLToPath(import.meta.url) === process.argv[1]) {
  const result = await cli(process.argv.slice(2), process.env, message => process.stderr.write(message + '\n')); process.stdout.write(JSON.stringify(result.output) + '\n'); process.exitCode = result.exitCode;
}
