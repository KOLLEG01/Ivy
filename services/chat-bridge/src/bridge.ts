import { canonical } from '../../../packages/sdk/src/node.js';
import { IvyError, requireThat } from '../../../packages/sdk/src/node.js';
import type { Chat, Wire } from '../../../packages/sdk/src/node.js';
import { validateChat } from '../../../packages/sdk/src/node.js';
import type { InvocationContext } from '../../../packages/sdk/src/service.js';
import catalog from '../../../specs/schemas/chat.operations.json' with { type: 'json' };
import { ChatMain } from './chat-main.js';
import { ChatCancellation } from './cancellation.js';
import { ChatOutbox } from './outbox.js';
import { ChatQueries } from './queries.js';
import type { ChatTool } from './registry.js';
import { ChatWorker } from './worker.js';
import { ChatNotices } from './notices.js';
import { ChatCollector } from './collector.js';
import type { CollectionRetry } from './collector.js';

/** One Hive connection generation; caller identity comes exclusively from provider invocation. */
export class ChatBridge {
  readonly outbox: ChatOutbox;
  readonly queries: ChatQueries;
  readonly cancellation: ChatCancellation;
  readonly worker: ChatWorker;
  constructor(readonly main: ChatMain) {
    this.outbox = new ChatOutbox(main.operations); this.queries = new ChatQueries(main.operations);
    this.cancellation = new ChatCancellation(main); this.worker = new ChatWorker(main);
  }
  async workspace(caller: string): Promise<Chat.WorkspaceInfo> {
    const { admission, native, store } = this.main, expectedBridge = admission.expected(caller);
    await native.verifyOwner();
    const main = await this.main.find(), binding = main?.value.binding ? await store.read('chat-bridge/binding', main.value.binding) : null;
    if (binding) {
      const value = binding.value, definition = admission.definition;
      requireThat(value.workspaceId === definition.workspaceId && value.definitionHash === admission.definitionHash &&
        canonical(value.project) === canonical(definition.project) && canonical(value.nativePlan) === canonical(definition.nativePlan) &&
        value.primary.serviceNodeId === definition.project.serviceNodeId && value.primary.namespace === 'codex' && value.primary.kind === 'thread',
        'chat_binding_mismatch', 'The selected Main must preserve its configured native project and owner.');
    }
    let nativeOwnerReady = false;
    try { await native.ready(); nativeOwnerReady = true; }
    catch (error) { if (!(error instanceof IvyError)) throw error; }
    // A native outage cannot hide saved history. Recheck our own generation separately.
    await native.verifyOwner();
    return { expectedBridge, project: structuredClone(admission.definition.project), main: main ? { object: main.pin, data: main.value } : null,
      binding: binding ? { object: binding.pin, data: binding.value } : null, channels: admission.channelsFor(caller), nativeOwnerReady, observedAt: new Date().toISOString() };
  }
  async invoke(name: ChatTool, args: Record<string, Wire.Json>, context: InvocationContext): Promise<Wire.Json> {
    const { callerPrincipalId: caller, generation, signal } = context;
    signal.throwIfAborted();
    requireThat(this.main.native.owner.generation === generation, 'stale_generation', 'ChatBridge invocation belongs to another Hive generation.');
    // Bind every trusted caller to the exact current bridge definition before domain I/O.
    this.main.admission.expected(caller, name === 'notify' || name === 'notice' ? 'notice' : name === 'operation' ? 'either' : 'reader');
    validateChat(catalog.operations[name].input.slice(8), args);
    if (catalog.operations[name].mutation) {
      requireThat(typeof args['operationId'] === 'string' && context.operationId === args['operationId'], 'invalid_arguments', 'The transport and Chat action must use one operation identity.');
      await this.main.store.validateOperationId(args['operationId']);
    }
    let result: unknown;
    switch (name) {
      case 'retryResult': result = await new ChatCollector(this.main).retry(caller, args as unknown as CollectionRetry); break;
      case 'workspace': result = await this.workspace(caller); break;
      case 'operation': {
        const request = args as unknown as Chat.OperationQuery;
        this.main.admission.authorize(caller, request.expectedBridge, undefined, 'either');
        const operation = await this.main.operations.find(caller, request.operationId);
        requireThat(operation, 'not_found', 'This caller has no retained ChatBridge operation with that identity.'); result = operation.value; break;
      }
      case 'createMain': result = await this.main.create(caller, args as unknown as Chat.CreateMainRequest); break;
      case 'send': result = await this.main.queue.send(caller, args as unknown as Chat.SendRequest); break;
      case 'cancel': result = await this.cancellation.cancel(caller, args as unknown as Chat.CancelRequest); break;
      case 'receive': result = await this.outbox.receive(caller, args as unknown as Chat.ReceiveRequest); break;
      case 'acknowledge': result = await this.outbox.acknowledge(caller, args as unknown as Chat.AcknowledgeRequest); break;
      case 'history': result = await this.outbox.history(caller, args as unknown as Chat.HistoryQuery); break;
      case 'input': result = await this.queries.input(caller, args as unknown as Chat.InputQuery); break;
      case 'notify': result = await new ChatNotices(this.main).notify(caller, args as unknown as Chat.NotifyRequest); break;
      case 'notice': result = await new ChatNotices(this.main).notice(caller, args as unknown as Chat.NoticeQuery); break;
    }
    signal.throwIfAborted(); return result as Wire.Json;
  }
}
