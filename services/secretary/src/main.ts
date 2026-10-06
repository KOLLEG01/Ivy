import { fileURLToPath } from 'node:url';
import { dirname, join, resolve } from 'node:path';
import { setTimeout as delay } from 'node:timers/promises';
import { ServiceClient, ServiceConnection, IvyError, hashJson } from '../../../packages/sdk/src/node.js';
import { configurationPath, instanceConfig, HealthFile } from '../../../packages/sdk/src/host.js';
import { need, secretaryRegistry, validate } from './schema.js';
import type { FollowUpRequest, GetAssignmentRequest, ListAssignmentsRequest, Request, SaveAssignmentRequest, SaveConfigurationRequest, Scope, Settings } from './schema.js';
import { SecretaryEngine } from './engine.js';
import { SecretaryNotices } from './notices.js';
import { loadTriageConfiguration } from './triage-config.js';
import { triageRegistry } from './triage-schema.js';
import { SecretaryFollowUp } from './follow-up.js';
import { loadMinimumMessageAgeMinutes } from './delivery-config.js';
import { AssignmentScheduler } from './assignment-scheduler.js';
import { AssignmentRunner } from './assignment-runner.js';

export async function startSecretary(path: string) {
  const config = await instanceConfig(path); validate('Settings', config.settings); const settings = config.settings as unknown as Settings;
  need(config.componentId === 'secretary' && config.credential && config.serviceNodeId === settings.identity.serviceNodeId && config.hostId === settings.identity.hostId, 'invalid_arguments', 'Secretary needs its own component, credential and exact configured host/service identity.');
  const serviceConfigPath=join(dirname(resolve(config.dataRoot)),'config.json');
  const minimumMessageAgeMinutes = await loadMinimumMessageAgeMinutes(serviceConfigPath);
  const triage = await loadTriageConfiguration(serviceConfigPath, settings);
  let assignmentScheduler: AssignmentScheduler | null = null, assignmentRunner: AssignmentRunner | null = null, followUpWorker: SecretaryFollowUp | null = null, activeStage = 'idle', wakePending = false;
  const health = new HealthFile(config); let engine: SecretaryEngine | null = null, closed = false, active: Promise<void> | null = null;
  let timer: NodeJS.Timeout | null = null, control: NodeJS.Timeout | null = null, failure: string | null = null, healthWork: Promise<void> = Promise.resolve();
  let lastReadyGeneration: number | null = null;
  const background = new Map<string, Promise<void>>();
  const launch = (name: string, selected: SecretaryEngine, work: () => Promise<void>) => {
    if (background.has(name)) return;
    const pending = work().then(() => { selected.recoveryIssues.delete(name); }).catch(error => {
      if (!selected.signal.aborted) selected.issue(name, IvyError.from(error).code);
    }).finally(() => { background.delete(name); });
    background.set(name, pending);
  };
  const writeHealth = (ready: boolean, detail: string, generation: number | null = null) => {
    healthWork = healthWork.catch(() => undefined).then(() => health.write(ready, detail, generation)); return healthWork;
  };
  const kick = () => {
    if (closed || !engine || engine.signal.aborted) return;
    const selectedBackground = engine;
    const selectedFollowUp = followUpWorker;
    if (selectedFollowUp) launch('follow-ups', selectedBackground, () => selectedFollowUp.tick());
    launch('notices', selectedBackground, async () => {
      const notices = new SecretaryNotices(selectedBackground, minimumMessageAgeMinutes);
      for (const id of await selectedBackground.noticePage()) {
        try { await notices.step(id); selectedBackground.recoveryIssues.delete(id); }
        catch (error) { if (selectedBackground.signal.aborted) throw error; selectedBackground.issue(id, IvyError.from(error).code); }
      }
    });
    if (active) { wakePending = true; return; } wakePending = false; if (timer) clearTimeout(timer); timer = null; const selected = engine;
    active = (async () => {
      activeStage = 'engine.tick'; await selected.tick();
      activeStage = 'assignments.schedule'; await assignmentScheduler?.tick();
      activeStage = 'assignments.run'; await assignmentRunner?.tick();
      // Automatic work runs only through assignments; historical context stays readable.
      selected.recoveryIssues.delete('service-tick'); failure = null; activeStage = 'idle';
    })().catch(error => {
      if (!selected.signal.aborted) {
        const issue = IvyError.from(error);
        selected.issue('service-tick', issue.code);
        process.stderr.write(JSON.stringify({ code: issue.code, stage: activeStage }) + '\n');
      }
    })
      .finally(() => { active = null; if (!closed && engine === selected && !selected.signal.aborted) timer = setTimeout(kick, wakePending ? 0 : settings.pollMs); });
  };
  let activeConnection: ServiceConnection | null = null, recovery: Promise<void> | null = null;
  const service = new ServiceClient({ publicBaseUrl: config.publicBaseUrl, credential: () => config.credential!,
    identity: { serviceNodeId: config.serviceNodeId, serviceName: 'secretary', instanceMode: 'singleton', hostId: config.hostId, version: config.version, buildId: config.buildId, hiveProtocol: 1 },
    registry: () => {
      const registry = secretaryRegistry(), empty = { contracts: [], requiredContracts: [] };
      const assessment = triage ? triageRegistry() : empty;
      return { ...registry, contracts: [...registry.contracts, ...assessment.contracts], requiredContracts: [...[...registry.requiredContracts, ...assessment.requiredContracts].map(required => ({ ...required, readScope: { roots: [settings.identity.scope.rootObjectId], history: 'all' as const, includeArchived: true, references: [] } }))] };
    }, heartbeatMs: 2000,
    handlers: { ...Object.fromEntries(['capture', 'assess', 'attach', 'checkpoint', 'operation', 'operationRead', 'status', 'binding', 'listAssignments', 'getAssignment', 'saveConfiguration', 'saveAssignment', 'createAssignment', 'updateAssignment'].map(name => ['secretary.' + name, async (args, context) => {
      need(engine && engine.owner.generation === context.generation, 'service_not_ready', 'Secretary needs its reconciled current generation.'); context.signal.throwIfAborted();
      if (name === 'binding') { engine.authorize(context.callerPrincipalId, settings.identity.scope, 'status'); await engine.verifyOwner();
        const binding = { serviceNodeId: config.serviceNodeId, hostId: config.hostId, available: true as const, expectedScope: settings.identity.scope, generation: context.generation };
        return { ...binding, bindingHash: hashJson(binding), observedAt: new Date().toISOString() } as never; }
      if (name === 'listAssignments') return await engine.listAssignments(context.callerPrincipalId, args as unknown as ListAssignmentsRequest) as never;
      if (name === 'getAssignment') return await engine.getAssignment(context.callerPrincipalId, args as unknown as GetAssignmentRequest) as never;
      if (name === 'saveConfiguration') { need(context.operationId === args['operationId'], 'invalid_arguments', 'The configuration and routed operation identities must agree.'); const outcome = await engine.saveConfiguration(context.callerPrincipalId, args as unknown as SaveConfigurationRequest); kick(); return outcome as never; }
      if (['saveAssignment', 'createAssignment', 'updateAssignment'].includes(name)) {
        need(context.operationId === args['operationId'], 'invalid_arguments', 'The assignment and routed operation identities must agree.');
        need(name === 'saveAssignment' || (name === 'createAssignment' ? args['assignment'] === null : args['assignment'] !== null), 'invalid_arguments', name === 'createAssignment' ? 'Creating an assignment requires assignment=null.' : 'Updating an assignment requires its current revision pin.');
        const outcome = await engine.saveAssignment(context.callerPrincipalId, args as unknown as SaveAssignmentRequest); kick(); return outcome as never;
      }
      if (name === 'operation') return await engine.operation(context.callerPrincipalId, args['expectedScope'] as unknown as Scope, String(args['operationId'])) as never;
      if (name === 'operationRead') return await engine.operationRead(context.callerPrincipalId, args['expectedScope'] as unknown as Scope, String(args['operationId'])) as never;
      if (name === 'status') return await engine.status(context.callerPrincipalId, args['expectedScope'] as unknown as Scope) as never;
      need(args['action'] === name && context.operationId === args['operationId'], 'invalid_arguments', 'The domain and routed action identities must agree.');
      const outcome = await engine.action(context.callerPrincipalId, args as unknown as Request); kick(); return outcome as never;
    }])), 'secretary.followUp': async (args, context) => {
      need(engine && engine.owner.generation === context.generation, 'service_not_ready', 'Secretary needs its reconciled item context.');
      need(context.operationId === args['operationId'], 'invalid_arguments', 'The follow-up and routed operation identities must agree.');
      const outcome = await new SecretaryFollowUp(engine).run(args as unknown as FollowUpRequest, context.callerPrincipalId); kick(); return outcome as never;
    }, 'secretary.followUpRead': async (args, context) => {
      need(engine && engine.owner.generation === context.generation, 'service_not_ready', 'Secretary is reconnecting.');
      return await new SecretaryFollowUp(engine).read(String(args['operationId']), context.callerPrincipalId) as never;
    } },
    reconcile: async connection => {
      activeConnection = connection;
      const work = (async () => {
        await active; await Promise.allSettled(background.values()); await assignmentRunner?.drain();
        engine?.store.close(); engine = null;
        for (let attempt = 0; ; attempt++) {
          let next: SecretaryEngine | null = null;
          try {
            next = new SecretaryEngine(connection, settings, { serviceNodeId: config.serviceNodeId, generation: connection.generation }, connection.signal, undefined, config.dataRoot);
            await next.initialize(); await next.tick(); connection.signal.throwIfAborted();
            const nextScheduler = new AssignmentScheduler(next); await nextScheduler.initialize();
            const nextRunner = new AssignmentRunner(next, minimumMessageAgeMinutes);
            assignmentScheduler = nextScheduler; assignmentRunner = nextRunner;
            connection.signal.throwIfAborted();
            followUpWorker = new SecretaryFollowUp(next); failure = null; engine = next;
            return;
          } catch (error) {
            next?.store.close();
            const issue = IvyError.from(error);
            if (issue.code !== 'limit_exceeded' || connection.signal.aborted || attempt >= 5) throw error;
            await delay(250 * (attempt + 1), undefined, { signal: connection.signal });
          }
        }
      })();
      recovery = work;
      void work.catch(error => {
        if (connection.signal.aborted) return;
        const issue = IvyError.from(error); failure = issue.code;
        process.stderr.write(JSON.stringify({ code: issue.code, phase: 'reconcile' }) + '\n');
        activeConnection?.close(issue);
      }).finally(() => { if (recovery === work) recovery = null; });
    },
    readiness: async connection => {
      const issue = failure ?? engine?.recoveryIssues.values().next().value ?? null;
      const ready = engine?.owner.generation === connection.generation && !failure && !recovery;
      await writeHealth(ready, issue ? 'Secretary recovery needs attention; independent inbox actions remain available.' : ready ? 'Secretary sources and original operations are reconciled.' : 'Secretary reconciliation is pending.', connection.generation);
      const at = new Date().toISOString(); return { ready, diagnostics: issue ? [{ code: issue, message: 'Secretary recovery requires attention; other source work continues.', severity: 'warning' as const, resource: null, source: config.serviceNodeId, firstObservedAt: at, lastObservedAt: at, status: 'current' as const }] : [] };
    },
    onState: state => {
      if (state.status === 'ready' && state.generation !== lastReadyGeneration) {
        lastReadyGeneration = state.generation ?? null; kick();
      }
      else if (state.status === 'degraded') lastReadyGeneration = null;
      else if (['offline', 'stopped', 'connecting'].includes(state.status)) { lastReadyGeneration = null; activeConnection = null; if (timer) clearTimeout(timer); timer = null;
        if (state.status === 'offline' && state.code) process.stderr.write(JSON.stringify({ code: state.code, phase: 'hive-connection' }) + '\n');
        void writeHealth(false, 'Secretary Hive connection is unavailable; original actions remain saved.').catch(() => undefined); }
    },
  });
  const close = async () => { if (closed) return; closed = true; if (timer) clearTimeout(timer); if (control) clearInterval(control); await service.stop(); await recovery?.catch(() => undefined); await active; await Promise.allSettled(background.values()); await assignmentRunner?.drain(); engine?.store.close(); engine = null; await healthWork.catch(() => undefined); await health.write(false, 'Secretary stopped.'); };
  let checking = false;
  try {
    await writeHealth(false, 'Connecting Secretary and recovering original actions.');
    control = setInterval(() => { if (checking || closed) return; checking = true; void health.control().then(value => value ? close() : undefined).catch(() => undefined).finally(() => { checking = false; }); }, 500);
    service.start(); return { service, close, get engine() { return engine; } };
  } catch (error) { await close(); throw error; }
}
if (process.argv[1] && fileURLToPath(import.meta.url) === process.argv[1]) {
  try { const secretary = await startSecretary(configurationPath()); const stop = () => { void secretary.close().then(() => { process.exitCode = 0; }, () => { process.exitCode = 1; }); }; process.on('SIGTERM', stop); process.on('SIGINT', stop); }
  catch (error) { process.stderr.write(JSON.stringify({ code: IvyError.from(error).code, message: 'Secretary failed to start.' }) + '\n'); process.exitCode = 1; }
}
