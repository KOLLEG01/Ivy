import { fileURLToPath } from 'node:url';
import { IvyError, requireThat, ServiceClient, validateTaskBoard } from '../../../packages/sdk/src/node.js';
import type { TaskBoard, ToolHandler, Wire } from '../../../packages/sdk/src/node.js';
import { configurationPath, HealthFile, instanceConfig } from '../../../packages/sdk/src/host.js';
import { TaskBoardEngine } from '../../../services/task-board/src/runtime/engine.js';
import { TaskBoardReconciler } from '../../../services/task-board/src/runtime/reconciler.js';
import { taskBoardRegistry, taskBoardTools } from '../../../services/task-board/src/runtime/registry.js';
import { TaskBoardStore } from '../../../services/task-board/src/runtime/store.js';

export async function startTaskBoard(path: string) {
  const config = await instanceConfig(path); validateTaskBoard('Settings', config.settings);
  requireThat(config.componentId === 'task-board' && config.credential, 'invalid_arguments', 'TaskBoard requires its own component identity and Hive credential.');
  const settings = config.settings as unknown as TaskBoard.Settings, health = new HealthFile(config);
  let closed = false, engine: TaskBoardEngine | null = null, runtime: TaskBoardReconciler | null = null;
  let startedGeneration: number | null = null, wakePending = false, lastTickStartedAt = 0, timerAt = 0;
  let timer: NodeJS.Timeout | null = null, control: NodeJS.Timeout | null = null, activeWork: Promise<void> | null = null, healthWork: Promise<void> = Promise.resolve();
  let retiring: Promise<void> = Promise.resolve();
  const retireRuntime = () => {
    const previous = runtime, previousEngine = engine;
    runtime = null; engine = null;
    const work = previous ? Promise.all([previous.stop(), activeWork]).then(() => undefined)
      .finally(() => previousEngine?.store.close()) : Promise.resolve();
    retiring = Promise.all([retiring, work]).then(() => undefined);
    return retiring;
  };
  const minimumTickIntervalMs = Math.min(250, settings.scheduler.intervalMs);
  const writeHealth = (ready: boolean, generation: number | null, detail: string) => {
    healthWork = healthWork.catch(() => undefined).then(() => health.write(ready, detail, generation)); return healthWork;
  };
  const schedule = (delay: number) => {
    const at = Date.now() + delay;
    if (timer && timerAt <= at) return;
    if (timer) clearTimeout(timer);
    timerAt = at;
    timer = setTimeout(() => { timer = null; timerAt = 0; kick(); }, delay);
  };
  const kick = () => {
    if (closed || !runtime || !service.ready) return;
    if (activeWork) { wakePending = true; return; }
    const delay = Math.max(0, minimumTickIntervalMs - (Date.now() - lastTickStartedAt));
    if (delay) { schedule(delay); return; }
    wakePending = false;
    if (timer) clearTimeout(timer); timer = null; timerAt = 0;
    const selected = runtime;
    lastTickStartedAt = Date.now();
    activeWork = selected.tick().finally(() => {
      activeWork = null;
      if (!closed) schedule(runtime?.recovered ? (wakePending ? 0 : settings.scheduler.intervalMs) : 50);
    });
  };
  const handlers: Record<string, ToolHandler> = {};
  for (const tool of taskBoardTools) handlers['task-board.' + tool.name] = async (args, context) => {
    requireThat(engine && runtime && engine.owner.generation === context.generation, 'service_not_ready', 'TaskBoard has no reconciled current connection generation.');
    if (tool.name === 'workspace') {
      await engine.verifyOwner(); context.signal.throwIfAborted();
      const result: TaskBoard.WorkspaceInfo = { ...engine.owner, principalId: settings.principalId, rootObjectId: settings.rootObjectId,
        callerPrincipalId: context.callerPrincipalId, role: 'user',
        recovered: runtime.recovered, observedAt: new Date().toISOString(), scheduler: settings.scheduler, phoneTarget: settings.phoneTarget };
      return result;
    }
    if (tool.name === 'operation') return await engine.operation(String(args['operationId']), context) as unknown as Wire.Json;
    if (tool.name === 'configuration') return await engine.configuration() as unknown as Wire.Json;
    if (tool.name === 'read') { await engine.verifyOwner(); return await engine.task(args as unknown as TaskBoard.TaskQuery, context.callerPrincipalId) as unknown as Wire.Json; }
    if (tool.name === 'list') { await engine.verifyOwner(); return await engine.list(args as unknown as TaskBoard.TaskListQuery) as unknown as Wire.Json; }
    if (tool.name === 'markCommentsRead') return await engine.markCommentsRead(args as unknown as TaskBoard.MarkCommentsReadRequest, context) as unknown as Wire.Json;
    if (tool.name === 'archive') { const result = await engine.archive(args as unknown as TaskBoard.ArchiveRequest, context); runtime.taskChanged(); return result as unknown as Wire.Json; }
    if (tool.name === 'categories') { await engine.verifyOwner(); return await engine.categories() as unknown as Wire.Json; }
    const request = args as unknown as TaskBoard.ActionInput;
    requireThat(runtime.recovered || !['start', 'continue'].includes(request.action), 'task_board_recovery_pending', 'Existing Operations and Runs are being reconciled before accepting fresh execution claims.');
    const outcome = await engine.invoke(request, context); runtime.taskChanged(); kick(); return outcome as unknown as Wire.Json;
  };
  const service = new ServiceClient({ publicBaseUrl: config.publicBaseUrl, credential: () => config.credential!,
    identity: { serviceNodeId: config.serviceNodeId, serviceName: 'task-board', instanceMode: 'singleton', hostId: config.hostId, version: config.version, buildId: config.buildId, hiveProtocol: 1 },
    registry: taskBoardRegistry, handlers, heartbeatMs: 2000,
    notificationFilters: () => [{ namespace: 'agent', name: 'notification', version: '1.0.0' }],
    onNotification: value => { if (runtime?.native.notification(value)) kick(); },
    reconcile: async connection => {
      await retireRuntime();
      const next = new TaskBoardEngine(new TaskBoardStore(connection, settings.rootObjectId, config.dataRoot), settings, { serviceNodeId: config.serviceNodeId, generation: connection.generation });
      await next.verifyOwner(); if (settings.rootObjectId) await connection.request('objects.stat', { objectId: settings.rootObjectId });
      engine = next; runtime = new TaskBoardReconciler(next);
    },
    readiness: async connection => {
      requireThat(engine && runtime && engine.owner.generation === connection.generation, 'service_not_ready', 'Workflow state is not attached to this generation.');
      await engine.verifyOwner();
      await writeHealth(true, connection.generation, runtime.recovered ? 'Workflow actions and native reconciliation are available.' : 'Workflow storage is available; existing work is being reconciled before fresh execution claims.');
      return { ready: true, diagnostics: runtime.diagnostics };
    },
    onState: state => {
      if (state.status === 'ready' && state.generation !== startedGeneration) { startedGeneration = state.generation ?? null; kick(); }
      else if (['offline', 'stopped', 'connecting'].includes(state.status)) {
        void retireRuntime(); startedGeneration = null; if (timer) clearTimeout(timer); timer = null; timerAt = 0;
        void writeHealth(false, state.generation ?? null, 'Hive workflow connection is not ready'+(state.code?': '+state.code:'')+'.').catch(() => undefined);
      }
    },
  });
  const close = async () => {
    if (closed) return; closed = true; if (timer) clearTimeout(timer); if (control) clearInterval(control);
    await service.stop(); await activeWork?.catch(() => undefined); await retireRuntime(); await healthWork.catch(() => undefined); await health.write(false, 'TaskBoard stopped.');
  };
  await writeHealth(false, null, 'Connecting TaskBoard and verifying its exact workflow contracts.');
  let checkingControl = false;
  control = setInterval(() => {
    if (closed || checkingControl) return; checkingControl = true;
    void health.control().then(value => value ? close() : undefined).catch(() => undefined).finally(() => { checkingControl = false; });
  }, 500);
  service.start(); return { service, close, get runtime() { return runtime; } };
}
if (process.argv[1] && fileURLToPath(import.meta.url) === process.argv[1]) {
  try {
    const taskBoard = await startTaskBoard(configurationPath());
    const stop = () => { void taskBoard.close().then(() => { process.exitCode = 0; }, () => { process.exitCode = 1; }); };
    process.on('SIGTERM', stop); process.on('SIGINT', stop);
  } catch (error) { process.stderr.write(JSON.stringify({ code: IvyError.from(error).code, message: 'TaskBoard failed to start.' }) + '\n'); process.exitCode = 1; }
}
