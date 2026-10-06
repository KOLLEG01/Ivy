import { parseArgs } from 'node:util';
import { resolve, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { setTimeout as delay } from 'node:timers/promises';
import { bootstrapOs } from './bootstrap-os.js';
import { inspectBootstrap, updateBootstrap } from './bootstrap-maintenance.js';
import { retainedBootstrapPlans } from './bootstrap.js';
import { hostConfig } from './host-config.js';
import { HostJournal } from './journal.js';
import { jsonFile } from './config.js';
import { servicePaths } from './layout.js';
import { hashJson } from '../../contracts/src/canonical.js';
import { IvyError, requireThat } from '../../contracts/src/errors.js';
import { validateHost } from '../../contracts/src/host-validation.js';
import type { Host } from '../../contracts/src/generated.js';
import { automaticBootstrapRecord, saveAutomaticBootstrapRecord } from './bootstrap-automatic.js';
import type { AutomaticBootstrapPhase } from './bootstrap-automatic.js';

function selectedCandidate(plan: Host.BootstrapPlan, instanceId: string): string | null {
  const process = plan.processes.find(value => value.instanceId === instanceId);
  return process ? process.candidateId ?? plan.candidateId : null;
}

async function matchingPlan(config: Host.HostConfig, snapshot: Host.BootstrapSnapshot): Promise<Host.BootstrapPlan> {
  const plans = await retainedBootstrapPlans(config);
  const plan = plans.find(value => snapshot.owners.every(owner => selectedCandidate(value, owner.instanceId) === owner.candidateId));
  requireThat(plan, 'not_found', 'The prior exact bootstrap plan is not retained.'); return plan;
}

async function readiness(config: Host.HostConfig, plan: Host.BootstrapPlan, states: Host.BootstrapSnapshot, notBefore: number, timeoutMs = 90_000): Promise<void> {
  const journal = new HostJournal(config);
  try {
    const expected = new Map(plan.processes.map(value => [value.instanceId, journal.candidate(value.candidateId ?? plan.candidateId).buildId]));
    const deadline = Date.now() + timeoutMs;
    while (true) {
      let ready = true;
      for (const state of states.owners.filter(value => value.running)) {
        const instance = config.instances.find(value => value.instanceId === state.instanceId); requireThat(instance, 'not_found', 'Bootstrap instance disappeared.');
        try {
          const health = await jsonFile<Host.Health>(join(servicePaths(config, instance).data, 'health.json')); validateHost('Health', health);
          const observedAt = Date.parse(health.observedAt);
          if (!health.ready || health.buildId !== expected.get(state.instanceId) || observedAt < notBefore || Date.now() - observedAt > 15_000) ready = false;
        } catch { ready = false; }
      }
      if (ready) return;
      requireThat(Date.now() < deadline, 'readiness_failed', 'Updated bootstrap owners did not become ready before the rollback deadline.');
      await delay(500);
    }
  } finally { journal.close(); }
}

export async function runAutomaticBootstrapUpdate(configPath: string, requestPath: string, distribution: string,
  os = bootstrapOs(resolve(distribution)), waitForReadiness = readiness): Promise<void> {
  const path = resolve(configPath), request = await jsonFile<Host.BootstrapMaintenanceRequest>(resolve(requestPath));
  validateHost('BootstrapMaintenanceRequest', request);
  let config = await hostConfig(path), record = await automaticBootstrapRecord(config, request.operationId);
  requireThat(record && record.requestHash === hashJson(request), 'storage_invalid', 'Automatic bootstrap request has no exact retained record.');
  if (record.phase === 'succeeded' || record.phase === 'rolled_back') return;
  requireThat(record.phase !== 'needs_attention', 'bootstrap_update_failed', 'Automatic bootstrap maintenance needs explicit attention.');
  requireThat(os.pause && os.resume, 'unsupported_runtime', 'This host cannot hand bootstrap ownership to offline maintenance.');
  const original = request.previous, prior = await matchingPlan(config, original);
  const startingPhase = record.phase;
  const save = async (phase: AutomaticBootstrapPhase, errorCode: string | null = null) => {
    record!.phase = phase; record!.errorCode = errorCode; await saveAutomaticBootstrapRecord(config, record!);
  };
  const sameDefinitions = (current: Host.BootstrapSnapshot) => current.owners.length === original.owners.length && current.owners.every(owner => {
    const expected = original.owners.find(value => value.instanceId === owner.instanceId && value.name === owner.name);
    return expected && expected.candidateId === owner.candidateId && expected.definitionHash === owner.definitionHash;
  });
  const rollback = async (failure: IvyError): Promise<void> => {
    await save('rolling_back', failure.code);
    config = await hostConfig(path); let plans = [...await retainedBootstrapPlans(config), request.next, prior];
    const current = await inspectBootstrap(path, resolve(distribution), os); await os.pause!(current, plans);
    const rollbackRequest: Host.BootstrapMaintenanceRequest = { schemaVersion: 1, operationId: request.operationId + '-rollback', previous: current, next: prior };
    validateHost('BootstrapMaintenanceRequest', rollbackRequest);
    const result = await updateBootstrap(path, rollbackRequest, resolve(distribution), os);
    requireThat(result.phase === 'succeeded', result.errorCode ?? 'bootstrap_rollback_failed', 'Automatic bootstrap rollback did not succeed.');
    config = await hostConfig(path); plans = await retainedBootstrapPlans(config);
    const resumedAt = Date.now(); await os.resume!(original, plans); await waitForReadiness(config, prior, original, resumedAt);
    await save('rolled_back', failure.code);
  };
  try {
    if (['updating', 'verifying', 'rolling_back'].includes(startingPhase)) {
      await rollback(new IvyError('outcome_unknown', 'An interrupted automatic bootstrap update is rolled back before normal execution resumes.', 'unknown'));
      return;
    }
    await save('checking');
    await os.pause(original, [...await retainedBootstrapPlans(config), request.next]);
    await save('updating');
    const result = await updateBootstrap(path, request, resolve(distribution), os);
    requireThat(result.phase === 'succeeded', result.errorCode ?? 'bootstrap_update_failed', 'Automatic bootstrap maintenance did not succeed.');
    config = await hostConfig(path); const updatedPlans = await retainedBootstrapPlans(config), resumedAt = Date.now();
    await os.resume(original, updatedPlans); await save('verifying'); await waitForReadiness(config, request.next, original, resumedAt);
    await save('succeeded'); return;
  } catch (error) {
    const failure = IvyError.from(error);
    try {
      if (record.phase === 'checking') {
        config = await hostConfig(path); const plans = [...await retainedBootstrapPlans(config), request.next, prior];
        const current = await inspectBootstrap(path, resolve(distribution), os);
        requireThat(sameDefinitions(current), 'target_conflict', 'Bootstrap ownership changed before automatic maintenance acquired it.');
        const resumedAt = Date.now(); await os.resume(original, plans); await waitForReadiness(config, prior, original, resumedAt);
        await save('rolled_back', failure.code); return;
      }
      await rollback(failure); return;
    } catch (rollbackError) {
      await save('needs_attention', IvyError.from(rollbackError).code); throw rollbackError;
    }
  }
}

if (process.argv[1] && fileURLToPath(import.meta.url) === process.argv[1]) {
  try {
    const { values } = parseArgs({ options: { config: { type: 'string' }, request: { type: 'string' }, distribution: { type: 'string' } } });
    requireThat(values.config && values.request && values.distribution, 'invalid_arguments', 'Automatic bootstrap worker requires config, request and distribution paths.');
    await runAutomaticBootstrapUpdate(values.config, values.request, values.distribution);
  } catch (error) { process.stderr.write(JSON.stringify({ code: IvyError.from(error).code, message: 'Automatic bootstrap update needs attention.' }) + '\n'); process.exitCode = 1; }
}
