import { fileURLToPath } from 'node:url';
import { consumeEvents, IvyError, requireThat, ServiceClient, validateAutomation } from '../../../../../packages/sdk/src/node.js';
import { configurationPath, HealthFile, instanceConfig } from '../../../../../packages/sdk/src/host.js';
import { AutomationEngine, automationRegistry } from './engine.js';

export async function startAutomation(path: string) {
  const config = await instanceConfig(path); validateAutomation('Settings', config.settings);
  requireThat(config.componentId === 'automation-example' && config.credential, 'invalid_arguments', 'The automation example requires its own component identity and credential.');
  const health = new HealthFile(config);
  let closed = false, readyGeneration: number | null = null;
  let engine: AutomationEngine | null = null, tickWork: Promise<void> = Promise.resolve();
  let control: NodeJS.Timeout | null = null, ticker: NodeJS.Timeout | null = null, checkingControl = false, healthWork: Promise<void> = Promise.resolve();
  const writeHealth = (ready: boolean, detail: string, generation: number | null = null) => {
    healthWork = healthWork.catch(() => undefined).then(() => health.write(ready, detail, generation)); return healthWork;
  };
  const service = new ServiceClient({ publicBaseUrl: config.publicBaseUrl, credential: () => config.credential!,
    identity: { serviceNodeId: config.serviceNodeId, hostId: config.hostId, serviceName: 'automation-example', version: config.version, buildId: config.buildId, hiveProtocol: 1 },
    registry: automationRegistry, handlers: {}, heartbeatMs: 2000,
    reconcile: async connection => {
      const next = new AutomationEngine(connection, config.settings as unknown as import('../../../../../packages/sdk/src/node.js').Automation.Settings);
      await next.initialize({ serviceNodeId: connection.serviceNodeId, generation: connection.generation });
      await next.tick(); engine = next; readyGeneration = connection.generation;
      void consumeEvents(connection, { name: next.definition.subscriptionName,
        filter: { topics: [next.definition.inputTopic], sources: [next.definition.inputSource] }, initialSequence: next.definition.initialSequence },
      (batch, signal) => next.persistEvents(batch, signal), { pollMs: next.settings.pollMs }).catch(error => {
        if (!connection.signal.aborted) connection.close(IvyError.from(error));
      });
    },
    readiness: async connection => {
      requireThat(readyGeneration === connection.generation && engine, 'service_not_ready', 'Automation example has not initialized its durable workflow.');
      await writeHealth(true, 'Automation schedule and durable event consumer are ready.', connection.generation);
      return { ready: true, diagnostics: [] };
    },
    onState: state => {
      if (['offline', 'stopped', 'connecting'].includes(state.status)) { readyGeneration = null; engine = null; void writeHealth(false, 'Hive connection is unavailable.').catch(() => undefined); }
    },
  });
  const close = async () => {
    if (closed) return; closed = true; if (control) clearInterval(control); if (ticker) clearInterval(ticker);
    await service.stop(); await healthWork.catch(() => undefined);
    await health.write(false, 'Automation example stopped.');
  };
  await writeHealth(false, 'Connecting the durable automation example.');
  control = setInterval(() => {
    if (closed || checkingControl) return; checkingControl = true;
    void health.control().then(value => value ? close() : undefined).catch(() => undefined).finally(() => { checkingControl = false; });
  }, 500);
  ticker = setInterval(() => {
    const current = engine;
    if (closed || !service.ready || !current) return;
    tickWork = tickWork.catch(() => undefined).then(async () => { await current.tick(); });
    void tickWork.catch(() => service.connection.close(new IvyError('automation_tick_failed', 'Automation tick failed.', 'unknown')));
  }, config.settings.pollMs as number);
  service.start(); return { service, close };
}
if (process.argv[1] && fileURLToPath(import.meta.url) === process.argv[1]) {
  try {
    const runtime = await startAutomation(configurationPath());
    const stop = () => { void runtime.close().then(() => { process.exitCode = 0; }, () => { process.exitCode = 1; }); };
    process.on('SIGTERM', stop); process.on('SIGINT', stop);
  } catch (error) { process.stderr.write(JSON.stringify({ code: IvyError.from(error).code, message: 'Automation example failed to start.' }) + '\n'); process.exitCode = 1; }
}
