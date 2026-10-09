import type { TestContext } from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, rmSync } from 'node:fs';
import { join, relative } from 'node:path';
import { tmpdir } from 'node:os';
import { PassThrough } from 'node:stream';
import { randomUUID } from 'node:crypto';
import { hashJson } from '../../packages/contracts/src/canonical.js';
import { IvyError } from '../../packages/contracts/src/errors.js';
import type { Agent, Wire } from '../../packages/contracts/src/generated.js';
import { NativeNotifications } from '../../services/agent-manager/src/notifications.js';
import { NativeJournal } from '../../services/agent-manager/src/journal.js';
import { NativeRpc } from '../../services/agent-manager/src/rpc.js';
import { catalogFor } from '../../services/task-board/src/runtime/evidence.js';
import { TaskBoardNativeDriver } from '../../services/task-board/src/runtime/native-driver.js';
import { nativeWorkflow } from './task-board-native.js';

export async function nativeDriverFixture(t: TestContext, nativeVersion: '0.154.0' = '0.154.0', maxJournalBytes = 64 * 1024 * 1024) {
  const w = await nativeWorkflow(t, nativeVersion), catalog = catalogFor(nativeVersion).catalog;
  const root = mkdtempSync(join(tmpdir(), 'ivy-native-driver-'));
  const limits = { maxOperations: 100, maxJournalBytes, maxPendingInputs: 16, maxNotificationBytes: 1048576 };
  const journal = new NativeJournal({ serviceNodeId: 'native-agent', hostId: 'fixture-host', nativeVersion: catalog.version, nativeExecutableHash: catalog.nativeExecutableHash }, limits);
  let notifications = new NativeNotifications(journal.owner, journal.reserveNotificationSequence(), limits.maxNotificationBytes);
  const frames: Record<string, Wire.Json>[] = []; let epoch = '', heldMethod: string | null = null, loseToolReply = false, genericAbsence = false, ownerAbsence = false, providerClosed = false, ownerCalls = 0;
  const processStops: NonNullable<Agent.Status['processStops']> = [];
  let nativeReply: ((frame: Record<string, Wire.Json>) => Agent.Reply | undefined) | null = null;
  let readBarrier: { method: string; remaining: number; ready: Promise<void>; release: () => void } | null = null;
  let input: PassThrough, output: PassThrough, rpc: NativeRpc;
  const launch = () => {
    epoch = randomUUID(); journal.beginEpoch(epoch); input = new PassThrough(); output = new PassThrough();
    rpc = new NativeRpc(catalog, input, output, { onClose: code => journal.loseEpoch(epoch, code), onRequest: () => undefined, onNotification: () => undefined });
    input.on('data', (bytes: Buffer) => {
      const frame = JSON.parse(bytes.toString('utf8')) as Record<string, Wire.Json>; frames.push(frame);
      if (frame['method'] === heldMethod) return;
      const overridden = nativeReply?.(frame);
      if (overridden) { output.write(JSON.stringify({ id: frame['id'], ...overridden }) + '\n'); return; }
      const result = frame['method'] === 'thread/read' ? { thread: w.nativeThread() } : frame['method'] === 'turn/start' ? { turn: w.turn() }
        : frame['method'] === 'turn/interrupt' ? {} : { thread: w.nativeThread(), cwd: '/fixture', model: 'fixture-model', modelProvider: 'openai',
          approvalPolicy: 'on-request', approvalsReviewer: 'user', sandbox: { type: 'readOnly' }, reasoningEffort: 'high' };
      output.write(JSON.stringify({ id: frame['id'], result }) + '\n');
    });
  };
  launch();
  t.after(() => { rpc.close('fixture_stop'); input.destroy(); output.destroy(); journal.close(); assert.ok(relative(tmpdir(), root).startsWith('ivy-native-driver-')); rmSync(root, { recursive: true, force: true }); });
  w.f.nativeTools(async call => {
    assert.equal(call.serviceNodeId, 'native-agent'); const args = call.arguments as Record<string, Wire.Json>;
    ownerCalls++;
    if (providerClosed) throw new IvyError('service_unavailable', 'Provider connection is closed.', 'not_executed');
    const identity = { callerPrincipalId: call.callerPrincipalId, operationId: String(args?.['operationId'] ?? call.operationId) };
    if (call.definition.namespace === 'agent' && call.definition.name !== 'invoke') {
      if (call.definition.name === 'catalog') return catalog as unknown as Wire.Json;
      if (call.definition.name === 'capabilities') return { serviceNodeId: 'native-agent', hostId: 'fixture-host', revision: 1,
        capabilities: [], updatedAt: new Date().toISOString(), operationId: null } satisfies Agent.CapabilityProfile;
      if (call.definition.name === 'resolveWorkspace') return w.planned.workspace as unknown as Wire.Json;
      if (call.definition.name === 'resolveProject') return { kind: 'task', cwd: w.planned.workspace.canonicalCwd,
        project: { nativeId: 'fixture-task-project', source: 'native', name: 'Fixture task', paths: [w.planned.workspace.canonicalCwd], kind: 'task' } } satisfies Agent.ProjectLocation;
      if (call.definition.name === 'operation') {
        if (genericAbsence) throw new IvyError('not_found', 'Generic routing absence.');
        const operation = ownerAbsence ? null : journal.get(identity); if (operation) return operation;
        throw new IvyError('not_found', 'Exact fixture owner absence.', 'not_executed', { kind: 'agent_operation_absent', operationId: identity.operationId, serviceNodeId: 'native-agent', epoch });
      }
      if (call.definition.name === 'status') return { serviceNodeId: 'native-agent', hostId: 'fixture-host', serverType: 'codex', nativeVersion: catalog.version,
        nativeExecutableHash: catalog.nativeExecutableHash, catalogHash: hashJson(catalog), epoch, pid: null, state: 'ready', observedAt: new Date().toISOString(), code: null,
        initialized: {}, pendingInputs: 0, operations: journal.status(), observedMethods: journal.observedMethods(), processStops,
        connection: { mode: 'owned-stdio', ownsServer: true, ownsHome: true, actualHome: root, actualVersion: catalog.version,
          socketPath: null, serverPid: null, lifecycle: 'owned-stdio', mcpIdentity: 'instance-environment' } } satisfies Agent.Status;
      if (call.definition.name === 'prevent') return journal.prevent(identity, String(args['method']), args['params']!);
      if (call.definition.name === 'inputs') return journal.inputs(args as Agent.PendingInputQuery);
      if (call.definition.name === 'notifications') return notifications.page(args as Agent.NotificationQuery,epoch);
      if (call.definition.name === 'read') {
        const request = call.arguments as Agent.ReadInput; let requestId: Agent.RequestId = '';
        const barrier = readBarrier;
        if (barrier?.method === request.method) {
          if (--barrier.remaining === 0) { readBarrier = null; barrier.release(); }
          await barrier.ready;
        }
        const reply = await rpc.request(request.method, request.params, { beforeSend: id => { requestId = id; } });
        return { schemaVersion: 1, observationId: randomUUID(), callerPrincipalId: call.callerPrincipalId, serviceNodeId: 'native-agent',
          nativeExecutableHash: catalog.nativeExecutableHash, catalogHash: hashJson(catalog), epoch, requestId,
          requestHash: hashJson({ method: request.method, params: request.params }), observedAt: new Date().toISOString(), ...request, reply } satisfies Agent.ReadObservation;
      }
      throw new Error('Unexpected fixture management method.');
    }
    const invoke = call.definition.namespace === 'agent' && call.definition.name === 'invoke';
    const method = invoke ? String(args['method']) : call.definition.name, nativeArgs = invoke ? args['params']! : call.arguments;
    assert.ok(call.operationId); const key = { callerPrincipalId: call.callerPrincipalId, operationId: call.operationId! };
    const accepted = journal.accept(key, method, nativeArgs);
    if (accepted.created) {
      const currentEpoch = epoch;
      const request = rpc.request(method, nativeArgs, { beforeSend: id => { journal.dispatch(key, currentEpoch, id); }, beforeResolve: (id, reply) => { journal.finish(key, currentEpoch, id, reply); } });
      if (method === heldMethod) { void request.catch(() => undefined); throw new IvyError('outcome_unknown', 'Fixture caller lost after native dispatch.', 'unknown'); }
      await request;
    }
    if (loseToolReply) { loseToolReply = false; throw new IvyError('outcome_unknown', 'Fixture lost tool reply after durable native completion.', 'unknown'); }
    const operation = journal.get(key)!;
    if (invoke) return operation;
    if (operation.reply && 'result' in operation.reply) return operation.reply.result;
    throw new IvyError(operation.code ?? 'native_operation_pending', 'Original native result is not successful.', operation.phase === 'failed' ? 'not_executed' : 'unknown');
  });
  return { w, journal, get notifications() { return notifications; }, frames, first: new TaskBoardNativeDriver(w.f.first), second: new TaskBoardNativeDriver(w.f.second), epoch: () => epoch,
    get ownerCalls() { return ownerCalls; }, providerClosed: (value: boolean) => { providerClosed = value; },
    reply: (handler: typeof nativeReply) => { nativeReply = handler; },
    synchronizeReads: (method: string) => {
      const gate = Promise.withResolvers<void>(); readBarrier = { method, remaining: 2, ready: gate.promise, release: gate.resolve };
    },
    confirmStopped: (stoppedEpoch = epoch) => { processStops.push({ epoch: stoppedEpoch, observedAt: new Date().toISOString() }); },
    restart: () => { rpc.close('fixture_restart'); input.destroy(); output.destroy(); notifications = new NativeNotifications(journal.owner, journal.reserveNotificationSequence(), limits.maxNotificationBytes); launch(); },
    hold: (method: string | null = 'turn/start') => { heldMethod = method; }, loseReply: () => { loseToolReply = true; }, absence: (generic: boolean) => { genericAbsence = generic; ownerAbsence = !generic; } };
}
