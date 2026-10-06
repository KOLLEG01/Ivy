import { randomUUID } from 'node:crypto';
import { canonical, hashJson } from '../../../../packages/sdk/src/node.js';
import { IvyError, requireThat } from '../../../../packages/sdk/src/node.js';
import type { Chat, TaskBoard, Wire } from '../../../../packages/sdk/src/node.js';
import { validateChat } from '../../../../packages/sdk/src/node.js';
import { mainChat, mainNotification, chatInterfaceVersion } from '../../../../packages/sdk/src/node.js';
import { callBound, discover, newOperationId } from '../../../../packages/sdk/src/client.js';
import { readableVersions as chatReadableVersions } from '../../../chat-bridge/src/schema.js';
import type { TaskBoardEngine } from './engine.js';
import { mutation } from './store.js';
import type { Document } from './store.js';

const same = (a: unknown, b: unknown) => canonical(a) === canonical(b);
const fixed = ({ schemaVersion, taskId, commentId, publication, requestedRoute, createdAt }: TaskBoard.Delivery) =>
  ({ schemaVersion, taskId, commentId, publication, requestedRoute, createdAt });

export const initialDelivery = (settings: TaskBoard.Settings, publication: TaskBoard.ObjectPin, taskId: string, commentId: string,
  route: 'chat' | 'phone', at: string): TaskBoard.Delivery => ({
  schemaVersion: 1, taskId, commentId, publication, requestedRoute: route, activeRoute: route, state: 'pending',
  chatTarget: null, phoneTarget: structuredClone(settings.phoneTarget), operationId: null,
  chatRequest: null, phoneRequest: null, phoneCallId: null, deliveryEvidence: null, code: null, createdAt: at, updatedAt: at,
});

/** User-facing delivery is technical recovery state. The only ticket interaction is the Comment
 * referenced by this record; Chat and Voice are live conduits back to that same ticket. */
export class TaskBoardDeliveries {
  constructor(readonly engine: TaskBoardEngine) {}
  get store() { return this.engine.store; }
  private async current(id: string): Promise<Document<'task-board/delivery'> | null> {
    const current = await this.store.read('task-board/delivery', id), value = current.value;
    if (value.state === 'confirmed') return current;
    const publication = await this.store.read('task-board/operation', value.publication.objectId);
    if (publication.value.phase !== 'succeeded') return null;
    const created = publication.value.writes.find(write => write.contractKey === 'task-board/delivery' &&
      same(write.outcome, { objectId: id, revision: 1 }));
    requireThat(created, 'task_board_delivery_mismatch', 'Delivery requires its original prepared publication.');
    const first = (await this.store.read('task-board/delivery', created.payload.object, false)).value;
    requireThat(same(fixed(value), fixed(first)) && first.state === 'pending' && first.operationId === null && first.chatRequest === null &&
      first.phoneRequest === null && first.phoneCallId === null && first.deliveryEvidence === null && first.publication.revision === 1,
    'task_board_delivery_mismatch', 'Delivery identity and original Comment route cannot change.');
    requireThat(publication.value.outcome?.task?.objectId === first.taskId, 'task_board_delivery_mismatch', 'Delivery requires its completed Task publication.');
    const { task, comment } = await this.comment(first);
    requireThat(same(comment.delivery, created.outcome) && task.publication === null,
      'task_board_delivery_mismatch', 'The completed Task must contain the exact delivered Comment.');
    requireThat(value.requestedRoute === 'phone' || value.activeRoute === 'chat', 'task_board_delivery_mismatch', 'Chat delivery cannot change to Phone.');
    if (value.phoneRequest) requireThat(value.requestedRoute === 'phone' && value.phoneTarget && value.phoneRequest.recipientId === value.phoneTarget.recipientId &&
      value.phoneRequest.route === 'voice' && value.phoneRequest.voicePrompt === this.voicePrompt(task, comment),
      'task_board_delivery_mismatch', 'Phone delivery must retain the exact Comment prompt and configured recipient.');
    if (value.chatRequest) requireThat(value.chatTarget && same(value.chatRequest.expectedBridge, value.chatTarget.expectedBridge) &&
      same(value.chatRequest.channel, value.chatTarget.channel) && same(value.chatRequest.source, task.lastHistory) && value.chatRequest.text === this.handoff(task, comment),
      'task_board_delivery_mismatch', 'Chat delivery must retain the exact Comment handoff and configured Main channel.');
    requireThat(!value.phoneCallId || value.phoneRequest, 'task_board_delivery_mismatch', 'A Phone call identity requires its retained original request.');
    requireThat(value.state === 'pending' || value.operationId === (value.activeRoute === 'phone' ? value.phoneRequest?.operationId : value.chatRequest?.operationId),
      'task_board_delivery_mismatch', 'Active delivery state must retain its exact provider operation identity.');
    return current;
  }
  private async update(current: Document<'task-board/delivery'>, next: TaskBoard.Delivery): Promise<void> {
    if (same({ ...current.value, updatedAt: next.updatedAt }, next)) return;
    await this.engine.verifyOwner();
    await this.store.write('task-board/delivery', next, mutation(current.pin.objectId, 'delivery:' + current.pin.revision + ':' + hashJson(next)),
      { objectId: current.pin.objectId, expectedRevision: current.pin.revision });
  }
  private async comment(value: TaskBoard.Delivery): Promise<{ task: TaskBoard.Task; comment: TaskBoard.Comment }> {
    const publication = await this.store.read('task-board/operation', value.publication.objectId);
    const written = publication.value.writes.find(write => write.contractKey === 'task-board/task' &&
      write.outcome?.objectId === value.taskId && same(write.outcome, publication.value.outcome?.task));
    requireThat(publication.value.phase === 'succeeded' && written,
      'task_board_delivery_mismatch', 'Delivery requires its original completed Task publication.');
    // The local Operation remains retained until delivery finishes, even after Hive prunes old
    // Task revisions or the user archives the ticket.
    const task = (await this.store.read('task-board/task', written.payload.object, false)).value;
    const comment = task.comments.find(item => item.commentId === value.commentId);
    requireThat(comment && task.lastHistory, 'task_board_delivery_mismatch', 'The routed Comment and its publication history are unavailable.');
    return { task, comment };
  }
  private handoff(task: TaskBoard.Task, comment: TaskBoard.Comment): string {
    const requests = comment.nativeInput ? '\nNative request identity: ' + JSON.stringify(comment.nativeInput.identity) : comment.requests.length ? '\nTyped requests: ' + JSON.stringify(comment.requests) : '';
    const instructions = comment.purpose === 'handoff' ? 'The task is ready for review. Notify the user of the delivery; no answer is required. ' : 'Discuss this important question with the user. ';
    return (`TaskBoard ${task.taskKey} has a new ticket comment from ${comment.authorKind}:\n\n${comment.body}${requests}\n\n` +
      instructions + `Tell the user about this comment now. Keep ${task.taskKey} and its context in this current Main conversation. ` +
      'When the user answers or adds relevant information, use the connected Ivy Hive TaskBoard tools: read the current Task revision and append one comment action to this ticket. ' +
      'A normal answer is one comment with authorKind=user and empty requests/responses. For a native request, answer through the owning AgentManager using the exact native request identity; TaskBoard records that response. For a retained typed request, attach the response referencing its exact requestId. ' +
      'Do not treat sending this message as the user response.').slice(0, 16384);
  }
  private voicePrompt(task: TaskBoard.Task, comment: TaskBoard.Comment): string {
    const requests = comment.nativeInput ? '\nNative request identity: ' + JSON.stringify(comment.nativeInput.identity) : comment.requests.length ? '\nTyped requests: ' + JSON.stringify(comment.requests) : '';
    const instructions = comment.purpose === 'handoff' ? 'The task is ready for review. Notify the user of the delivery; no answer is required. ' : 'Discuss this important question with the user. ';
    return (`You are the Voice task for a call about TaskBoard ${task.taskKey}. This call concerns the following ticket comment:\n\n` +
      `${comment.body}${requests}\n\n${instructions}When the user answers or supplies relevant information, use the connected Ivy Hive TaskBoard tools: ` +
      `read ${task.taskKey} at its current revision and append one comment action. A normal answer is one comment with authorKind=user and empty requests/responses; ` +
      'a native request must be answered through its owning AgentManager with its exact identity; TaskBoard records that response. The ticket is the source of truth and wakes the working task. Do not involve Main chat.').slice(0, 4096);
  }
  private async chatCall(name: 'notify' | 'notice' | 'operation', target: TaskBoard.ChatTarget, args: Wire.Json, operationId?: string): Promise<Wire.Json> {
    await this.engine.verifyOwner();
    const node = await this.store.client.request('serviceNodes.get', { serviceNodeId: target.serviceNodeId });
    requireThat(node.serviceName === 'chat-bridge' && node.principalId === target.expectedBridge.principalId && node.connected && node.synced && node.ready,
      'task_board_chat_unavailable', 'The configured ChatBridge provider is not ready under its original principal.');
    const bound = await discover(this.store.client, 'chat.' + name,
      { serviceNodeId: target.serviceNodeId, interfaceVersion: chatInterfaceVersion });
    return callBound(this.store.client, bound, args, operationId);
  }
  private async phoneCall(name: 'request' | 'operation', target: TaskBoard.PhoneTarget, args: Wire.Json, operationId?: string): Promise<Wire.Json> {
    await this.engine.verifyOwner();
    const node = await this.store.client.request('serviceNodes.get', { serviceNodeId: target.serviceNodeId });
    requireThat(node.serviceName === 'phone-bridge' && node.connected && node.synced && node.ready,
      'task_board_phone_unavailable', 'The configured PhoneBridge provider is not ready.');
    const bound = await discover(this.store.client, 'phone.' + name,
      { serviceNodeId: target.serviceNodeId, interfaceVersion: '1.0.0' });
    return callBound(this.store.client, bound, args, operationId);
  }
  private async chatEvidence(delivery: TaskBoard.Delivery, pin: Chat.ObjectPin): Promise<Chat.ReplyView> {
    const raw = await this.store.client.request('objects.read', pin);
    requireThat(raw.object.contractKey === 'chat-bridge/reply' && chatReadableVersions.includes(raw.revision.contractVersion) && raw.content.encoding === 'json',
      'task_board_delivery_mismatch', 'Delivery proof must be an exact saved ChatBridge reply revision.');
    validateChat('Reply', raw.content.value); const reply = raw.content.value as Chat.Reply;
    requireThat(reply.origin.kind === 'native' && delivery.chatRequest, 'task_board_delivery_mismatch', 'The service handoff must be written by current Main.');
    const expected = delivery.chatRequest.expectedBridge;
    requireThat(reply.workspaceId === expected.workspaceId && same(reply.channel, delivery.chatRequest.channel),
      'task_board_delivery_mismatch', 'The Main reply must retain the original Chat identity.');
    const input = await this.store.client.request('objects.read', { objectId: reply.origin.inputId });
    requireThat(input.object.contractKey === 'chat-bridge/input' && chatReadableVersions.includes(input.revision.contractVersion) && input.content.encoding === 'json',
      'task_board_delivery_mismatch', 'The Main reply must retain its hidden TaskBoard handoff.');
    validateChat('Input', input.content.value); const handoff = input.content.value as Chat.Input;
    requireThat(handoff.definitionHash === reply.definitionHash && handoff.operationId === delivery.chatRequest.operationId && handoff.identity.senderPrincipalId === this.engine.settings.principalId &&
      same(handoff.identity.channel, delivery.chatRequest.channel) && same(handoff.binding, reply.origin.binding) && same(handoff.result, reply.origin.result),
      'task_board_delivery_mismatch', 'The Main reply must identify this exact TaskBoard handoff.');
    return { object: pin, data: reply };
  }
  private async chat(current: Document<'task-board/delivery'>): Promise<void> {
    const value = current.value;
    const target = value.chatRequest ? value.chatTarget : await mainChat(this.store.client);
    requireThat(target, 'task_board_delivery_mismatch', 'The original service handoff must retain its owner.');
    if (!value.chatRequest) {
      requireThat(target.expectedBridge.callerPrincipalId === this.engine.settings.principalId, 'task_board_delivery_mismatch', 'Chat configuration must identify TaskBoard as producer.');
      const { task, comment } = await this.comment(value), operationId = await newOperationId(this.store.client);
      const request: Chat.AdmittedNotifyRequest = { action: 'notify', operationId, expectedBridge: target.expectedBridge, channel: target.channel,
        source: task.lastHistory!, text: this.handoff(task, comment) };
      validateChat('AdmittedNotifyRequest', request);
      await this.update(current, { ...value, activeRoute: 'chat', state: 'dispatching', chatTarget: target, operationId, chatRequest: request,
        code: null, updatedAt: new Date().toISOString() }); return;
    }
    let reply: Chat.ReplyView | null = null;
    try {
      let result: Wire.Json;
      try { result = await this.chatCall('operation', target, { expectedBridge: value.chatRequest.expectedBridge, operationId: value.chatRequest.operationId } as Wire.Json); }
      catch (error) {
        if (!(error instanceof IvyError && error.code === 'not_found')) throw error;
        result = await this.chatCall('notify', target, mainNotification(value.chatRequest) as unknown as Wire.Json, value.chatRequest.operationId);
      }
      validateChat('Operation', result); const operation = result as Chat.Operation;
      requireThat(operation.request.action === 'notify' && same(mainNotification(operation.request), mainNotification(value.chatRequest)) && operation.callerPrincipalId === this.engine.settings.principalId,
        'task_board_delivery_unresolved', 'Reconcile the original Main handoff identity.');
      if (!same(operation.request, value.chatRequest)) {
        await this.update(current, { ...value, chatRequest: operation.request,
          chatTarget: { serviceNodeId: target.serviceNodeId, expectedBridge: operation.request.expectedBridge, channel: operation.request.channel },
          state: 'dispatching', code: 'awaiting_main', updatedAt: new Date().toISOString() }); return;
      }
      if (operation.phase === 'succeeded' && operation.outcome?.action === 'notify') {
        const found = await this.chatCall('notice', target, { operationId: value.chatRequest.operationId } as Wire.Json);
        validateChat('ReplyView', found); reply = found as Chat.ReplyView;
      } else {
        requireThat(['accepted', 'applying'].includes(operation.phase), 'task_board_delivery_unresolved', 'Main cannot complete the original handoff.');
        if (value.code !== 'awaiting_main' || value.state !== 'dispatching') await this.update(current, { ...value, state: 'dispatching', code: 'awaiting_main', updatedAt: new Date().toISOString() }); return;
      }
      requireThat(reply, 'task_board_delivery_unresolved', 'The original Main handoff has no reply.');
      const verified = await this.chatEvidence(value, reply.object);
      const state = verified.data.state === 'confirmed' ? 'confirmed' : verified.data.state === 'outcome_unknown' ? 'outcome_unknown' : 'dispatching';
      const code = state === 'confirmed' ? null : 'awaiting_display';
      await this.update(current, { ...value, state, deliveryEvidence: reply.object, code, updatedAt: new Date().toISOString() });
    } catch (error) { await this.uncertain(current, error); }
  }
  private async phone(current: Document<'task-board/delivery'>): Promise<void> {
    const value = current.value, target = value.phoneTarget ?? this.engine.settings.phoneTarget;
    if (!target) return this.fallback(current, 'task_board_phone_unconfigured');
    if (!value.phoneRequest) {
      const { task, comment } = await this.comment(value), operationId = randomUUID();
      const request: TaskBoard.PhoneDeliveryRequest = { operationId, recipientId: target.recipientId, route: 'voice', voicePrompt: this.voicePrompt(task, comment) };
      await this.update(current, { ...value, phoneTarget: target, state: 'dispatching', operationId, phoneRequest: request, code: null, updatedAt: new Date().toISOString() }); return;
    }
    if (!value.phoneCallId) {
      try {
        const result = await this.phoneCall('request', target, value.phoneRequest as unknown as Wire.Json, value.phoneRequest.operationId) as Record<string, unknown>;
        requireThat(result['operationId'] === value.phoneRequest.operationId && typeof result['callId'] === 'string', 'task_board_delivery_mismatch', 'Phone admission changed its original identity.');
        await this.update(current, { ...value, phoneCallId: result['callId'], code: 'awaiting_phone', updatedAt: new Date().toISOString() }); return;
      } catch (error) {
        const failure = IvyError.from(error); if (failure.outcome === 'not_executed') return this.fallback(current, failure.code);
        return this.uncertain(current, error);
      }
    }
    const dial = await this.phoneCall('operation', target, { callId: value.phoneCallId, method: 'call.dial' }) as Record<string, unknown> | null;
    if (!dial || dial['phase'] === 'submitted') { if (value.code !== 'awaiting_phone') await this.update(current, { ...value, code: 'awaiting_phone', updatedAt: new Date().toISOString() }); return; }
    if (dial['phase'] === 'outcome_unknown') return this.uncertain(current, new IvyError('phone_operation_outcome_unknown', 'The original phone connection outcome is unknown.', 'unknown'));
    const receipt = dial['receipt'] as Record<string, unknown> | null, observation = receipt?.['result'] as Record<string, unknown> | null;
    if (receipt?.['ok'] !== true || observation?.['state'] !== 'connected') return this.fallback(current, 'phone_not_answered');
    const prompted = await this.phoneCall('operation', target, { callId: value.phoneCallId, method: 'call.promptVoice', generation: 0 }) as Record<string, unknown> | null;
    if (!prompted || prompted['phase'] === 'submitted') { if (value.code !== 'awaiting_voice_prompt') await this.update(current, { ...value, code: 'awaiting_voice_prompt', updatedAt: new Date().toISOString() }); return; }
    const promptReceipt = prompted['receipt'] as Record<string, unknown> | null;
    if (prompted['phase'] === 'outcome_unknown' || promptReceipt?.['ok'] !== true)
      return this.uncertain(current, new IvyError('phone_prompt_outcome_unknown', 'The connected Voice prompt outcome is unknown.', 'unknown'));
    await this.update(current, { ...value, state: 'confirmed', code: null, updatedAt: new Date().toISOString() });
  }
  private async fallback(current: Document<'task-board/delivery'>, code: string): Promise<void> {
    await this.update(current, { ...current.value, activeRoute: 'chat', state: 'pending', chatTarget: null, operationId: null, chatRequest: null,
      deliveryEvidence: null, code, updatedAt: new Date().toISOString() });
  }
  private async uncertain(current: Document<'task-board/delivery'>, error: unknown): Promise<void> {
    const failure = IvyError.from(error);
    if (['revision_conflict', 'mutation_conflict'].includes(failure.code)) throw error;
    if (current.value.state !== 'outcome_unknown' || current.value.code !== failure.code)
      await this.update(current, { ...current.value, state: 'outcome_unknown', code: failure.code, updatedAt: new Date().toISOString() });
  }
  async step(id: string): Promise<void> {
    await this.engine.verifyOwner(); const current = await this.current(id);
    if (!current || current.value.state === 'confirmed') return;
    if (current.value.state === 'outcome_unknown') {
      // Unknown external effects are only re-read through their exact saved identity below.
      if (!(current.value.activeRoute === 'phone' && current.value.phoneRequest || current.value.activeRoute === 'chat' && current.value.chatRequest)) return;
    }
    if (current.value.activeRoute === 'phone') await this.phone(current); else await this.chat(current);
  }
}
