import { join, resolve } from 'node:path';
import { readdir } from 'node:fs/promises';
import { bootstrapOs } from './bootstrap-os.js';
import type { BootstrapDefinition, BootstrapOs, BootstrapOwner } from './bootstrap-os.js';
import { configurationBootstrapPlan, bootstrapComponents, retainedBootstrapPlans, retainBootstrapPlan, linuxUnit, publishBootstrapInstanceConfigurations } from './bootstrap.js';
import { hostConfig } from './host-config.js';
import { HostJournal, ExecutorLock } from './journal.js';
import { atomicJson, jsonFile } from './config.js';
import { verifyCandidate } from './artifact.js';
import { offlineBootstrapOwners } from './private-backup.js';
import { copyVerified, exists, privateDirectory } from './backup-files.js';
import { hashJson, digest } from '../../contracts/src/canonical.js';
import { IvyError, requireThat } from '../../contracts/src/errors.js';
import { validateHost } from '../../contracts/src/host-validation.js';
import type { Host } from '../../contracts/src/generated.js';
import { resolveHostConfiguration } from './layout.js';

const currentHostConfiguration = async (path: string) => resolveHostConfiguration(await jsonFile<Host.HostConfigInput>(path), path);

function operationRoot(config: Host.HostConfig, operationId: string): string {
  requireThat(operationId.length > 0 && operationId.length <= 128, 'invalid_arguments', 'A bounded stable bootstrap operation ID is required.');
  return join(config.runtimeRoot, 'bootstrap-maintenance', hashJson(operationId).slice(7));
}
async function retained(config: Host.HostConfig, operationId: string): Promise<Host.BootstrapMaintenanceRecord | null> {
  const path = join(operationRoot(config, operationId), 'record.json');
  if (!await exists(path)) return null;
  const record = await jsonFile<Host.BootstrapMaintenanceRecord>(path); validateHost('BootstrapMaintenanceRecord', record);
  requireThat(record.operationId === operationId && record.request.operationId === operationId && record.requestHash === hashJson(record.request), 'storage_invalid', 'Bootstrap operation identity is inconsistent.');
  return record;
}
export async function bootstrapStatus(config: Host.HostConfig, operationId: string): Promise<Host.BootstrapMaintenanceRecord> {
  const record = await retained(config, operationId); requireThat(record, 'not_found', 'Bootstrap maintenance operation was not found.'); return record;
}
async function candidatePlans(config: Host.HostConfig, configPath: string, journal: HostJournal, next?: Host.BootstrapPlan): Promise<Host.BootstrapPlan[]> {
  const rows = journal.db.prepare('SELECT candidate_json FROM candidates WHERE component_id=? ORDER BY candidate_id LIMIT 1001').all('host-executor');
  requireThat(rows.length > 0 && rows.length <= 1000, 'limit_exceeded', 'Expected a bounded set of retained host-executor candidates.');
  const plans = rows.flatMap(row => {
    const candidate = JSON.parse(String(row['candidate_json'])) as Host.Candidate; validateHost('Candidate', candidate);
    try {
      journal.manifest(candidate.candidateId);
    } catch (error) {
      // The journal may retain obsolete, unselected candidates whose manifests
      // predate today's required fields. They cannot supply a new OS plan; exact
      // already-retained plans are checked independently below.
      if (IvyError.from(error).code === 'invalid_arguments') return [];
      throw error;
    }
    return [configurationBootstrapPlan(config, configPath, candidate, undefined, bootstrapComponents(config, journal, candidate))];
  });
  for (const prior of [...await retainedBootstrapPlans(config), ...(next ? [next] : [])]) {
    let candidate: Host.Candidate;
    try { candidate = journal.candidate(prior.candidateId); }
    catch (error) { if (IvyError.from(error).code === 'not_found') continue; throw error; }
    if (candidate.componentId !== 'host-executor') continue;
    requireThat(prior.artifactRoot === candidate.artifactRoot && prior.configPath === configPath && prior.nodeExecutable === config.executables['node'],
      'target_conflict', 'Retained bootstrap plan differs from this candidate or host paths.');
    // Retained bootstrap plans are already the exact per-process release map.
    // Rebuilding them from today's installed pointers would silently change an
    // older OS definition before maintenance has compared it.
    plans.push(prior);
  }
  return [...new Map(plans.map(plan => [hashJson(plan), plan])).values()];
}
function publicOwner(owner: BootstrapOwner): Host.BootstrapSnapshot['owners'][number] {
  // OS launch metadata (for example allowWindowsBreakaway) is part of the hashed
  // definition, not an extra field of the public ownership snapshot.
  const { instanceId, componentId, name, candidateId, definitionHash, enabled, running } = owner;
  return { instanceId, componentId, name, candidateId, definitionHash, enabled, running };
}
async function inspectOwned(os: BootstrapOs, plans: Host.BootstrapPlan[], journal: HostJournal, verifyArtifacts = true): Promise<BootstrapOwner[]> {
  const owners = await os.inspect(plans), instances = plans[0]!.processes;
  requireThat(owners.length === instances.length && new Set(owners.map(owner => owner.instanceId)).size === instances.length &&
    owners.every(owner => instances.some(instance => instance.instanceId === owner.instanceId && instance.name === owner.name) && owner.definition.hash === owner.definitionHash),
    'target_conflict', 'Observed bootstrap owners differ from the configured instance set.');
  if (verifyArtifacts) for (const id of new Set(owners.map(owner => owner.candidateId))) await verifyCandidate(journal.candidate(id), journal.config);
  return owners;
}
function offline(config: Host.HostConfig, journal: HostJournal, owners: BootstrapOwner[]): void {
  requireThat(journal.unfinished().length === 0, 'bootstrap_busy', 'Finish or reconcile host activations before bootstrap maintenance.');
  // offlineOwners holds every executor/runtime-owner lock and checks native process ownership.
  // Paused OS scheduling is sufficient; preserve the desired service state for the next boot.
  requireThat(owners.every(owner => !owner.running && (process.platform !== 'win32' || !owner.enabled)), 'bootstrap_busy', 'Stop owned OS processes and disable Windows scheduler entries before bootstrap maintenance.');
}
export async function inspectBootstrap(configPath: string, distribution: string, os: BootstrapOs = bootstrapOs(distribution)): Promise<Host.BootstrapSnapshot> {
  const path = resolve(configPath), config = await hostConfig(path), journal = new HostJournal(config);
  try {
    // A status snapshot describes OS ownership, not artifact integrity. Maintenance
    // independently verifies all relevant candidate bytes before changing definitions.
    const plans = await candidatePlans(config, path, journal), owners = await inspectOwned(os, plans, journal, false);
    requireThat(hashJson(await currentHostConfiguration(path)) === hashJson(config), 'configuration_changed', 'Host configuration changed during bootstrap inspection.');
    const snapshot: Host.BootstrapSnapshot = { schemaVersion: 1, hostId: config.hostId, installationId: plans[0]!.installationId,
      configPath: path, configurationHash: hashJson(config), observedAt: new Date().toISOString(), owners: owners.map(publicOwner) };
    validateHost('BootstrapSnapshot', snapshot); return snapshot;
  } finally { journal.close(); }
}
export async function updateBootstrap(configPath: string, request: Host.BootstrapMaintenanceRequest, distribution: string,
  os: BootstrapOs = bootstrapOs(distribution)): Promise<Host.BootstrapMaintenanceRecord> {
  validateHost('BootstrapMaintenanceRequest', request);
  const path = resolve(configPath), config = await hostConfig(path), root = operationRoot(config, request.operationId), requestHash = hashJson(request);
  const lock = new ExecutorLock(join(config.runtimeRoot, 'bootstrap-maintenance-lock'));
  let closeOwners: (() => void) | null = null, journal: HostJournal | null = null, record: Host.BootstrapMaintenanceRecord | null = null;
  try {
    record = await retained(config, request.operationId);
    requireThat(!record || record.requestHash === requestHash, 'operation_conflict', 'Bootstrap operation ID already belongs to another exact request.');
    // Historical replay never inspects, repairs or rolls back definitions changed by a later operation.
    if (record?.phase === 'succeeded' || record?.errorCode === 'restore_reconciliation_required') return record;
    const { previous, next } = request;
    requireThat(next.configPath === path && previous.configPath === path && next.hostId === config.hostId && previous.hostId === config.hostId &&
      next.runtimeRoot === config.runtimeRoot && next.configHash === hashJson(config) && previous.configurationHash === hashJson(config),
      'configuration_changed', 'Bootstrap maintenance belongs to a different installation or configuration.');
    journal = new HostJournal(config);
    const candidate = journal.candidate(next.candidateId);
    requireThat(candidate.componentId === 'host-executor' && next.os === process.platform, 'target_conflict', 'Maintenance requires this platform\'s checked host-executor candidate.');
    const manifest = await verifyCandidate(candidate, config);
    const preferred = await Promise.all(next.processes.filter(value => value.componentId !== 'host-executor' && value.candidateId).map(async value => {
      const selected = journal!.candidate(value.candidateId!);
      requireThat(selected.componentId === value.componentId, 'target_conflict', 'Bootstrap process release belongs to another component.');
      return { candidate: selected, manifest: await verifyCandidate(selected, config) };
    }));
    requireThat(hashJson(next) === hashJson(configurationBootstrapPlan(config, path, candidate, next.linuxResources?.budget,
      bootstrapComponents(config, journal, candidate, [{ candidate, manifest }, ...preferred]))), 'target_conflict', 'Next bootstrap plan differs from the verified candidate/configuration.');
    requireThat(previous.installationId === next.installationId && previous.owners.length === next.processes.length &&
      new Set(previous.owners.map(owner => owner.instanceId)).size === next.processes.length &&
      previous.owners.every(owner => next.processes.some(instance => instance.instanceId === owner.instanceId && instance.name === owner.name)),
      'target_conflict', 'Bootstrap instance identities or OS names changed.');
    if (!record) {
      const parent = join(config.runtimeRoot, 'bootstrap-maintenance');
      if (!await exists(parent)) await privateDirectory(parent);
      if (!await exists(root)) await privateDirectory(root);
      const now = new Date().toISOString();
      record = { schemaVersion: 1, operationId: request.operationId, requestHash, request, createdAt: now, updatedAt: now, phase: 'checking', errorCode: null, definitions: [] };
      await atomicJson(join(root, 'record.json'), record);
    }
    closeOwners = await offlineBootstrapOwners(config, next.processes.map(value => value.instanceId));
    // Serialize durable lifecycle acceptances as well as OS ownership while definitions are replaced.
    journal.db.exec('BEGIN IMMEDIATE');
    const plans = await candidatePlans(config, path, journal, next);
    const owners = await inspectOwned(os, plans, journal); offline(config, journal, owners);
    requireThat(next.linuxResources || !owners.some(owner => plans.some(plan => plan.linuxResources && digest(linuxUnit(plan, owner)) === owner.definitionHash)),
      'resource_scope_required', 'Bootstrap maintenance cannot remove an established Linux resource boundary.');
    requireThat(hashJson(await currentHostConfiguration(path)) === next.configHash, 'configuration_changed', 'Host configuration changed before bootstrap publication.');
    await retainBootstrapPlan(next);
    await os.prepare?.(next);
    const evidence = new Map<string, { prior: BootstrapDefinition; desired: BootstrapDefinition }>();
    if (record.definitions.length === 0) {
      for (const owner of owners) {
        const prior = previous.owners.find(value => value.instanceId === owner.instanceId)!;
        requireThat(owner.definitionHash === prior.definitionHash && owner.candidateId === prior.candidateId, 'target_conflict', 'OS bootstrap changed after its original snapshot.');
        const desired = await os.render(owner, next); await os.check(desired);
        for (const [name, definition] of [['prior', owner.definition], ['desired', desired]] as const) {
          const file = join(root, owner.instanceId + '.' + name + '.json');
          if (await exists(file)) requireThat(hashJson(await jsonFile(file)) === hashJson(definition), 'storage_invalid', 'Incomplete retained definition evidence differs from this request.');
          else await atomicJson(file, definition);
        }
        evidence.set(owner.instanceId, { prior: owner.definition, desired });
      }
      record.definitions = owners.map(owner => ({ instanceId: owner.instanceId, name: owner.name, previousHash: owner.definitionHash,
        desiredHash: evidence.get(owner.instanceId)!.desired.hash, applied: false }));
    } else {
      requireThat(record.definitions.length === owners.length && new Set(record.definitions.map(value => value.instanceId)).size === owners.length,
        'storage_invalid', 'Retained bootstrap definition set is incomplete.');
      for (const definition of record.definitions) {
        const previousOwner = previous.owners.find(owner => owner.instanceId === definition.instanceId && owner.name === definition.name);
        requireThat(previousOwner && previousOwner.definitionHash === definition.previousHash, 'storage_invalid', 'Retained bootstrap definition identity changed.');
        const prior = await jsonFile<BootstrapDefinition>(join(root, definition.instanceId + '.prior.json'));
        const desired = await jsonFile<BootstrapDefinition>(join(root, definition.instanceId + '.desired.json'));
        await os.check(prior); await os.check(desired);
        requireThat(prior.hash === definition.previousHash && desired.hash === definition.desiredHash &&
          (await os.render({ ...previousOwner, definition: prior }, next)).hash === desired.hash, 'storage_invalid', 'Retained definitions are not the exact old/new request.');
        evidence.set(definition.instanceId, { prior, desired });
      }
    }
    for (const owner of owners) {
      const definition = record.definitions.find(value => value.instanceId === owner.instanceId)!;
      requireThat([definition.previousHash, definition.desiredHash].includes(owner.definitionHash), 'target_conflict', 'An OS definition has a third state; maintenance will not overwrite it.');
    }
    const save = async () => { record!.updatedAt = new Date().toISOString(); validateHost('BootstrapMaintenanceRecord', record); await atomicJson(join(root, 'record.json'), record); };
    record.phase = 'updating'; record.errorCode = null; await save();
    for (const definition of record.definitions) {
      requireThat(hashJson(await currentHostConfiguration(path)) === next.configHash, 'configuration_changed', 'Host configuration changed during bootstrap maintenance.');
      const retainedDefinition = evidence.get(definition.instanceId)!;
      const original = previous.owners.find(owner => owner.instanceId === definition.instanceId)!;
      await os.apply({ ...original, definition: retainedDefinition.prior }, retainedDefinition.desired, plans);
      definition.applied = true; await save();
    }
    await os.reload();
    const after = await inspectOwned(os, plans, journal); offline(config, journal, after);
    requireThat(after.every(owner => owner.candidateId === next.processes.find(process => process.instanceId === owner.instanceId)!.candidateId! &&
      owner.definitionHash === evidence.get(owner.instanceId)!.desired.hash), 'target_conflict', 'OS definitions did not retain the selected bootstrap.');
    requireThat(hashJson(await currentHostConfiguration(path)) === next.configHash, 'configuration_changed', 'Host configuration changed before the final bootstrap outcome.');
    await publishBootstrapInstanceConfigurations(next, config, journal);
    await atomicJson(join(config.runtimeRoot, 'bootstrap', next.installationId + '.json'), next);
    journal.db.exec('COMMIT'); journal.syncBootstrap(next);
    record.phase = 'succeeded'; await save(); return record;
  } catch (error) {
    // Request conflicts and preflight errors must not rewrite an earlier operation's evidence.
    if (!record || record.requestHash !== requestHash || record.phase === 'succeeded') throw error;
    record.phase = 'needs_attention'; record.updatedAt = new Date().toISOString(); record.errorCode = IvyError.from(error).code;
    validateHost('BootstrapMaintenanceRecord', record); await atomicJson(join(root, 'record.json'), record); return record;
  } finally { if (journal?.db.isTransaction) journal.db.exec('ROLLBACK'); journal?.close(); closeOwners?.(); lock.close(); }
}

/** Only the new working copy is fenced; historical request paths and successful records are immutable. */
export async function holdRestoredBootstrapMaintenance(config: Host.HostConfig, at: string): Promise<number> {
  const parent = join(config.runtimeRoot, 'bootstrap-maintenance'); if (!await exists(parent)) return 0;
  let bytes = 0;
  for (const entry of await readdir(parent, { withFileTypes: true })) {
    requireThat(entry.isDirectory() && !entry.isSymbolicLink() && /^[0-9a-f]{64}$/.test(entry.name), 'storage_invalid', 'Invalid restored bootstrap maintenance directory.');
    const path = join(parent, entry.name, 'record.json'); if (!await exists(path)) continue;
    const record = await jsonFile<Host.BootstrapMaintenanceRecord>(path); validateHost('BootstrapMaintenanceRecord', record);
    requireThat(hashJson(record.operationId).slice(7) === entry.name && record.requestHash === hashJson(record.request), 'storage_invalid', 'Restored bootstrap identity is invalid.');
    if (record.phase === 'succeeded') continue;
    const original = join(config.runtimeRoot, 'recovery-original/bootstrap-maintenance', entry.name, 'record.json');
    bytes += (await copyVerified(path, original)).bytes;
    record.phase = 'needs_attention'; record.errorCode = 'restore_reconciliation_required'; record.updatedAt = at;
    await atomicJson(path, record);
  }
  return bytes;
}
