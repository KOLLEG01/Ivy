import { canonical, hashJson } from '../../../packages/sdk/src/node.js';
import { IvyError, requireThat } from '../../../packages/sdk/src/node.js';
import { validateChat } from '../../../packages/sdk/src/node.js';
import type { Chat, Wire } from '../../../packages/sdk/src/node.js';
import { ChatNativeDriver, nativePath } from './native-driver.js';
import { ChatOperations, contention } from './operations.js';
import { ChatQueue, conversationName, mainName } from './queue.js';
import { mutation } from './store.js';
import type { Document } from './store.js';
import { ChatCancellation } from './cancellation.js';
import { ChatNotices } from './notices.js';
import { resolveChatPlan } from './native-plan.js';
import { deriveOperationId } from '../../../packages/sdk/src/node.js';

type MainDocument = Document<'chat-bridge/main'>;
type OperationDocument = Document<'chat-bridge/operation'>;
const same = (a: unknown, b: unknown) => canonical(a) === canonical(b);
const bindingName = (operationId: string) => 'Chat binding ' + operationId;

/** Shared Main creation/replacement. Incoming messages may idempotently create the first Main. */
export class ChatMain {
  readonly queue: ChatQueue;
  constructor(readonly operations: ChatOperations, readonly native: ChatNativeDriver) {
    this.queue = new ChatQueue(operations, (caller, operationId) => this.ensure(caller, operationId, 'reader'));
    requireThat(operations.store === native.store && same(operations.admission.definition, native.admission.definition),
      'chat_scope_mismatch', 'Main and native dispatch must share the same store and immutable configuration.');
  }
  get store() { return this.operations.store; }
  get admission() { return this.operations.admission; }
  async find(): Promise<MainDocument | null> {
    const main = await this.store.named('chat-bridge/main', mainName(this.admission.definition));
    return main ? this.queue.checkMain(main) : null;
  }
  private async writeMain(main: MainDocument, next: Chat.Main, scope: string, step: string): Promise<MainDocument> {
    const pin = await this.store.write('chat-bridge/main', next, mutation(scope, step + ':' + main.pin.revision + ':' + hashJson(next)),
      { objectId: main.pin.objectId, expectedRevision: main.pin.revision });
    return this.store.read('chat-bridge/main', pin);
  }
  private checkAvailable(main: MainDocument | null, request: Chat.CreateMainRequest): void {
    requireThat((main?.pin.revision ?? null) === request.expectedMainRevision, 'revision_conflict', 'Explicit Main creation requires the observed aggregate revision.');
    requireThat(!main?.value.pendingAction, 'chat_pending_action', 'The original pending Main action must finish before replacement.');
    requireThat(!main?.value.queue.length, 'chat_main_busy', 'Main replacement requires an empty execution queue.');
  }
  private async resumeBinding(request: Chat.CreateMainRequest): Promise<Document<'chat-bridge/binding'> | null> {
    if (!request.resumeMain) return null;
    const source = await this.store.read('chat-bridge/main', request.resumeMain.objectId), definition = this.admission.definition;
    requireThat(same(source.pin, request.resumeMain), 'revision_conflict', 'Main resumption requires the observed source revision.');
    requireThat(!source.value.pendingAction && !source.value.publication && source.value.queue.length === 0,
      'chat_main_busy', 'Main resumption requires an unoccupied source Main.');
    requireThat(source.value.definitionHash !== this.admission.definitionHash && source.value.definitionHash === hashJson(source.value.definition) &&
      source.value.definition.principalId === definition.principalId && source.value.definition.workspaceId === definition.workspaceId &&
      source.value.definition.rootObjectId === definition.rootObjectId && same(source.value.definition.project, definition.project),
      'chat_definition_mismatch', 'Main resumption must retain the same principal, workspace and native project across configurations.');
    requireThat(source.value.binding, 'chat_binding_mismatch', 'Main resumption requires an existing source binding.');
    const binding = await this.store.read('chat-bridge/binding', source.value.binding);
    requireThat(binding.value.definitionHash === source.value.definitionHash && binding.value.workspaceId === definition.workspaceId &&
      same(binding.value.project, definition.project) && same(binding.value.nativePlan, source.value.definition.nativePlan) &&
      binding.value.primary.serviceNodeId === definition.project.serviceNodeId && binding.value.primary.namespace === 'codex' && binding.value.primary.kind === 'thread',
      'chat_binding_mismatch', 'Main resumption must retain its original definition-owned native thread.');
    return binding;
  }
  private async conversation(createdAt: string): Promise<Chat.ObjectPin> {
    const name = conversationName(this.admission.definition), existing = await this.store.named('chat-bridge/conversation', name);
    if (existing) {
      requireThat(existing.value.workspaceId === this.admission.definition.workspaceId && existing.value.definitionHash === this.admission.definitionHash,
        'chat_definition_mismatch', 'The retained conversation belongs to different ChatBridge settings.');
      return existing.pin;
    }
    const value: Chat.Conversation = { schemaVersion: 1, workspaceId: this.admission.definition.workspaceId, definitionHash: this.admission.definitionHash, createdAt };
    return this.store.write('chat-bridge/conversation', value, mutation(name, 'create'), { create: { parentId: this.store.rootObjectId, name } });
  }
  async create(caller: string, original: Chat.CreateMainRequest, role: 'reader' | 'notice' | 'either' = 'reader'): Promise<Chat.Operation> {
    this.admission.authorize(caller, original.expectedBridge, undefined, role); validateChat('CreateMainRequest', original);
    const request = structuredClone(original); await this.native.verifyOwner();
    if (!await this.operations.find(caller, request.operationId)) {
      await this.recoverPending(); this.checkAvailable(await this.find(), request); await this.resumeBinding(request); await this.native.verifyProject();
    }
    const accepted = await this.operations.begin(caller, request), scope = accepted.pin.objectId;
    for (let attempt = 0; attempt < 32; attempt++) {
      const current = await this.operations.find(caller, request.operationId), main = await this.find();
      requireThat(current, 'chat_identity_conflict', 'The original Main operation must remain discoverable.');
      if (main?.value.pendingAction?.objectId === scope) {
        await this.recoverPending(); return (await this.operations.find(caller, request.operationId))!.value;
      }
      if (['succeeded', 'failed'].includes(current.value.phase)) return current.value;
      requireThat(current.value.nativeCall === null && current.value.phase === 'accepted', 'chat_operation_unresolved', 'An applied Main action cannot acquire a new claim.');
      try { this.checkAvailable(main, request); }
      catch (error) {
        if (!(error instanceof IvyError)) throw error;
        return (await this.operations.rejectUnapplied(scope, error.code, error.message)).value;
      }
      try {
        await this.native.verifyOwner();
        if (main) await this.writeMain(main, { ...main.value, pendingAction: accepted.pin, updatedAt: accepted.value.createdAt }, scope, 'claim-main');
        else {
          const definition = this.admission.definition, name = mainName(definition);
          const conversation = await this.conversation(accepted.value.createdAt);
          await this.store.write('chat-bridge/main', { schemaVersion: 1, definition, definitionHash: this.admission.definitionHash,
            conversation, binding: null, pendingAction: accepted.pin, publication: null, queue: [], nextSequence: 1, updatedAt: accepted.value.createdAt }, mutation(name, 'create'),
          { create: { parentId: this.store.rootObjectId, name } });
        }
      } catch (error) { if (!contention(error)) throw error; }
    }
    throw new IvyError('chat_contention', 'Main changed repeatedly; recover this original creation action.', 'unknown');
  }
  /** Select the current Main or idempotently create the first one for an incoming message. */
  async ensure(caller: string, parentOperationId: string, role: 'reader' | 'notice' = 'reader'): Promise<Chat.BindingView> {
    const expectedBridge = this.admission.expected(caller, role);
    const operationId = deriveOperationId(parentOperationId, { action: 'chat:auto-main', caller });
    for (let attempt = 0; attempt < 8; attempt++) {
      const prior = await this.operations.find(caller, operationId);
      if (prior) {
        requireThat(prior.value.request.action === 'createMain', 'chat_identity_conflict', 'Automatic Main creation retained another action.');
        if (prior.value.phase === 'succeeded') {
          const outcome = prior.value.outcome;
          requireThat(outcome?.action === 'createMain', 'chat_identity_conflict', 'Automatic Main creation needs its exact binding receipt.');
          return structuredClone(outcome.binding);
        }
        if (prior.value.phase === 'failed' && prior.value.error?.code === 'revision_conflict') {
          let winner = await this.find();
          if (!winner?.value.binding) { await this.recoverPending(); winner = await this.find(); }
          requireThat(winner?.value.binding, 'chat_contention', 'The concurrent automatic Main winner has not published its binding.');
          const binding = await this.store.read('chat-bridge/binding', winner.value.binding);
          return { object: binding.pin, data: binding.value };
        }
        requireThat(prior.value.phase !== 'failed', prior.value.error?.code ?? 'chat_operation_failed', prior.value.error?.detail ?? 'Automatic Main creation failed.');
        await this.recoverPending();
        continue;
      }
      await this.recoverPending();
      const current = await this.find();
      if (current?.value.binding) {
        const binding = await this.store.read('chat-bridge/binding', current.value.binding);
        return { object: binding.pin, data: binding.value };
      }
      const request: Chat.CreateMainRequest = { action: 'createMain', operationId, expectedBridge,
        expectedMainRevision: current?.pin.revision ?? null, reason: 'Automatically create Main for the first incoming message.' };
      try {
        const created = await this.create(caller, request, role);
        if (created.phase === 'succeeded' && created.outcome?.action === 'createMain') return structuredClone(created.outcome.binding);
        requireThat(created.phase !== 'failed', created.error?.code ?? 'chat_operation_failed', created.error?.detail ?? 'Automatic Main creation failed.');
      } catch (error) {
        if (!(error instanceof IvyError && ['revision_conflict', 'chat_pending_action'].includes(error.code))) throw error;
      }
    }
    throw new IvyError('chat_contention', 'Automatic Main creation changed repeatedly; recover its original operation.', 'unknown');
  }
  private async claimed(scope: string, callId?: string): Promise<{ main: MainDocument; operation: OperationDocument }> {
    const main = await this.queue.main();
    requireThat(main.value.pendingAction?.objectId === scope && main.value.queue.length === 0,
      'revision_conflict', 'Native Main creation requires its original claim and an empty queue.');
    const original = await this.store.read('chat-bridge/operation', main.value.pendingAction), operation = await this.store.read('chat-bridge/operation', scope);
    this.operations.check(original.value); this.operations.check(operation.value);
    requireThat(original.value.request.action === 'createMain' && original.value.phase === 'accepted' && original.value.nativeCall === null &&
      original.value.callerPrincipalId === operation.value.callerPrincipalId && original.value.operationId === operation.value.operationId &&
      same(original.value.request, operation.value.request) && original.value.createdAt === operation.value.createdAt,
      'chat_identity_conflict', 'The claimed Main action must preserve its original caller, request, timestamp and attached native call.');
    requireThat(!callId || operation.value.phase === 'applying' && operation.value.nativeCall?.objectId === callId,
      'revision_conflict', 'The current action must still authorize this attached native call.');
    return { main, operation };
  }
  private async attach(scope: string, call: Document<'chat-bridge/native-call'>): Promise<OperationDocument> {
    for (let attempt = 0; attempt < 16; attempt++) {
      const { operation } = await this.claimed(scope), current = operation.value;
      requireThat(!current.nativeCall || current.nativeCall.objectId === call.pin.objectId, 'chat_native_mismatch', 'Main creation cannot switch to a different native call.');
      if (current.nativeCall && current.nativeCall.revision >= call.pin.revision) return operation;
      requireThat(!['succeeded', 'failed'].includes(current.phase), 'chat_operation_unresolved', 'A completed Main action cannot change its original native call.');
      let error: Chat.Error | null = null;
      if (call.value.state === 'failed' || call.value.state === 'outcome_unknown') error = { code: call.value.code!, detail: 'The original native Main creation ' + call.value.state + '.',
        outcome: call.value.state === 'failed' && call.value.epoch === null ? 'not_executed' : 'unknown' };
      const next: Chat.Operation = { ...current, nativeCall: call.pin, phase: error ? call.value.state as 'failed' | 'outcome_unknown' : 'applying',
        error, updatedAt: call.value.updatedAt };
      try {
        const pin = await this.store.write('chat-bridge/operation', next, mutation(scope, 'attach:' + operation.pin.revision + ':' + hashJson(next)),
          { objectId: scope, expectedRevision: operation.pin.revision });
        return this.store.read('chat-bridge/operation', pin);
      } catch (error) { if (!contention(error)) throw error; }
    }
    throw new IvyError('chat_contention', 'Recover the original Main native attachment.', 'unknown');
  }
  private async predecessor(main: MainDocument, operation: Chat.Operation): Promise<Chat.ObjectPin | null> {
    requireThat(operation.request.action === 'createMain', 'chat_identity_conflict', 'A Main binding needs its original creation action.');
    const revision = operation.request.expectedMainRevision;
    if (revision === null) return null;
    const base = await this.store.read('chat-bridge/main', { objectId: main.pin.objectId, revision });
    requireThat(base.value.definitionHash === this.admission.definitionHash && same(base.value.definition, this.admission.definition) &&
      !base.value.pendingAction && base.value.queue.length === 0 && revision < main.pin.revision,
      'chat_identity_conflict', 'Main replacement must retain its exact unoccupied original aggregate revision.');
    return base.value.binding;
  }
  async recoverPending(): Promise<void> {
    await this.native.verifyOwner();
    for (let attempt = 0; attempt < 32; attempt++) {
      const found = await this.find(); if (!found?.value.pendingAction) return;
      const pending = await this.store.read('chat-bridge/operation', found.value.pendingAction); this.operations.check(pending.value);
      if (pending.value.request.action === 'send') { await this.queue.recoverPending(); return; }
      if (pending.value.request.action === 'cancel') { await new ChatCancellation(this).recoverPending(); return; }
      if (pending.value.request.action === 'notify') { await new ChatNotices(this).recoverPending(); return; }
      requireThat(pending.value.request.action === 'createMain', 'chat_pending_action', 'Another pending Main action needs its own recovery.');
      const scope = pending.pin.objectId;
      try {
        let { main, operation } = await this.claimed(scope);
        if (operation.value.phase === 'failed') {
          requireThat(operation.value.nativeCall, 'chat_identity_conflict', 'A failed claimed creation must retain its original native call.');
          const failed = await this.native.get(scope, operation.value.nativeCall);
          requireThat(failed.value.state === 'failed', 'chat_operation_unresolved', 'Only an exact original native failure releases Main.');
          await this.writeMain(main, { ...main.value, pendingAction: null, updatedAt: operation.value.updatedAt }, scope, 'release-failure'); return;
        }
        if (operation.value.phase === 'succeeded') {
          const outcome = operation.value.outcome;
          requireThat(outcome?.action === 'createMain' && same(main.value.binding, outcome.binding.object) &&
            same((await this.store.read('chat-bridge/binding', outcome.binding.object)).value, outcome.binding.data),
            'chat_identity_conflict', 'The completed Main action must retain its published binding and receipt.');
          await this.writeMain(main, { ...main.value, pendingAction: null, updatedAt: operation.value.updatedAt }, scope, 'release-completed'); return;
        }
        requireThat(operation.value.request.action === 'createMain', 'chat_identity_conflict', 'Main creation must retain its original action.');
        const replaced = await this.predecessor(main, operation.value), resumed = await this.resumeBinding(operation.value.request);
        const previous = resumed?.pin ?? replaced, plan = await resolveChatPlan(this.store, this.admission.definition.nativePlan);
        const method = resumed ? 'thread/resume' : 'thread/start';
        const request: Chat.NativeRequest = { nativeVersion: plan.nativeVersion, catalogSourceHash: plan.catalogSourceHash,
          method, params: resumed ? { ...plan.threadResume, threadId: resumed.value.primary.nativeId } : structuredClone(plan.threadStart) } as Chat.NativeRequest;
        let call = await this.native.prepare(scope, deriveOperationId(operation.value.operationId, 'chat:native:' + method), request,
          undefined, resumed ? { excludeTurns: true } : undefined);
        operation = await this.attach(scope, call); // Persisted attachment always precedes native I/O.
        if (operation.value.phase === 'outcome_unknown') return;
        call = await this.native.advance(scope, call.pin, async () => {
          await this.native.verifyProject(); await this.claimed(scope, call.pin.objectId);
          await this.resumeBinding(operation.value.request as Chat.CreateMainRequest);
        });
        operation = await this.attach(scope, call);
        if (call.value.state === 'failed') continue;
        if (call.value.state !== 'succeeded') return;
        requireThat(call.value.evidence, 'chat_native_mismatch', 'A Main binding requires original native evidence.');
        const receipt = await this.native.evidence.operation(scope, call.value, call.value.evidence);
        requireThat(receipt.reply && 'result' in receipt.reply, 'chat_native_mismatch', 'Native Main creation needs its original successful reply.');
        const thread = (receipt.reply.result as Record<string, Wire.Json>)['thread'] as Record<string, Wire.Json>;
        requireThat(typeof thread['id'] === 'string' && thread['ephemeral'] === false && typeof thread['cwd'] === 'string' &&
          (!resumed || thread['id'] === resumed.value.primary.nativeId) &&
          nativePath(thread['cwd']) === nativePath((plan.threadStart as Record<string, Wire.Json>)['cwd'] as string),
          'chat_project_mismatch', 'The original native reply must identify a persistent Main in its configured cwd.');
        const definition = this.admission.definition, value: Chat.Binding = { schemaVersion: 1, workspaceId: definition.workspaceId,
          definitionHash: this.admission.definitionHash, project: definition.project, primary: { serviceNodeId: definition.project.serviceNodeId,
            namespace: 'codex', kind: 'thread', nativeId: thread['id'] }, nativePlan: definition.nativePlan, createdAt: operation.value.createdAt,
          operationId: operation.value.operationId, predecessor: previous };
        const name = bindingName(scope);
        let binding = await this.store.named('chat-bridge/binding', name);
        if (!binding) {
          const pin = await this.store.write('chat-bridge/binding', value, mutation(scope, 'publish-binding'), { create: { parentId: this.store.rootObjectId, name } });
          binding = await this.store.read('chat-bridge/binding', pin);
        }
        requireThat(binding.pin.revision === 1 && same(binding.value, value), 'chat_identity_conflict', 'The original Main binding must remain immutable.');
        ({ main, operation } = await this.claimed(scope));
        requireThat(same(main.value.binding, replaced) || same(main.value.binding, binding.pin), 'chat_identity_conflict', 'Pending Main creation cannot overwrite an unrelated binding.');
        if (!same(main.value.binding, binding.pin)) main = await this.writeMain(main, { ...main.value, binding: binding.pin, updatedAt: operation.value.createdAt }, scope, 'publish-main');
        await this.operations.finish(scope, { action: 'createMain', operationId: operation.value.operationId, binding: { object: binding.pin, data: binding.value } });
        await this.writeMain(main, { ...main.value, pendingAction: null, updatedAt: operation.value.createdAt }, scope, 'release-main'); return;
      } catch (error) { if (!contention(error)) throw error; }
    }
    throw new IvyError('chat_contention', 'Main publication changed repeatedly; recover its original action.', 'unknown');
  }
}
