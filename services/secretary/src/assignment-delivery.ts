import { callBound, hashJson, IvyError, newOperationId, serviceTools, mainChat, mainNotification, chatInterfaceVersion } from '../../../packages/sdk/src/node.js';
import type { Chat, Wire } from '../../../packages/sdk/src/node.js';
import { need, same } from './schema.js';
import type { ContactRule, Execution, Pin } from './schema.js';
import type { SecretaryEngine } from './engine.js';
import type { Document } from './store.js';
import { assignmentName, effectiveRules } from './assignment-schema.js';
import { AssignmentVoice } from './assignment-voice.js';
import { secretaryBrowserNotice } from './browser-notice.js';

export interface AssignmentDeliveryTarget { serviceNodeId: string; channel: Chat.Channel }
export interface AssignmentDecision { schemaVersion: 1; urgency: 'low' | 'normal' | 'high' | 'critical'; notification: 'none' | 'main' | 'voice'; text: string; reason: string }
export interface Handoff { executionId: string; resultHash: string; operationId: string | null; target?: AssignmentDeliveryTarget | null; request: Chat.AdmittedNotifyRequest | null; state: 'queued' | 'dispatching' | 'confirmed' | 'suppressed' | 'failed' | 'outcome_unknown'; reason: string | null; evidence: Pin | null; expiresAt: number | null; attempts?: number; retryAt?: number; terminal?: boolean }
const key = 'secretary/assignment-handoff';
const rank = { low: 0, normal: 1, high: 2, critical: 3 } as const;
const maxDeliveryAttempts = 5;

export function assignmentDecision(answer: string): AssignmentDecision {
  let value: unknown;
  const trimmed = answer.trim();
  const fenced = /^```(?:json)?\s*\n([\s\S]*?)\n```$/i.exec(trimmed);
  try { value = JSON.parse(fenced ? fenced[1]! : trimmed); } catch { throw new IvyError('secretary_assignment_result_invalid', 'The assignment did not return its required decision.'); }
  need(value && typeof value === 'object' && !Array.isArray(value), 'secretary_assignment_result_invalid', 'The assignment decision must be an object.');
  const row = value as Record<string, unknown>;
  need(row.schemaVersion === 1 &&
    typeof row.urgency === 'string' && ['low', 'normal', 'high', 'critical'].includes(row.urgency) &&
    typeof row.notification === 'string' && ['none', 'main', 'voice'].includes(row.notification) &&
    typeof row.text === 'string' && row.text.length <= 8192 && typeof row.reason === 'string' && row.reason.length <= 1024 &&
    (row.notification === 'none' || row.text.trim().length > 0) && (row.notification !== 'voice' || row.urgency === 'critical'),
    'secretary_assignment_result_invalid', 'The assignment decision is incomplete.');
  return { schemaVersion: 1, urgency: row.urgency as AssignmentDecision['urgency'], notification: row.notification as AssignmentDecision['notification'],
    text: row.text as string, reason: row.reason as string };
}

export function contactWindowOpen(rule: ContactRule, now: Date): boolean {
  if (!rule.window) return true;
  const { timeZone, start, end } = rule.window;
  const parts = new Intl.DateTimeFormat('en', { timeZone, hour: '2-digit', minute: '2-digit', hourCycle: 'h23', numberingSystem: 'latn' }).formatToParts(now);
  const at = parts.find(value => value.type === 'hour')!.value + ':' + parts.find(value => value.type === 'minute')!.value;
  return start < end ? at >= start && at < end : at >= start || at < end;
}

/** The original operation and recipient are saved before any external handoff. */
export class AssignmentDelivery {
  readonly voice: AssignmentVoice;
  constructor(readonly engine: SecretaryEngine, readonly minimumNotificationAgeMinutes = 0) {
    this.voice = new AssignmentVoice(engine);
  }

  private async bound(name: 'notify' | 'notice' | 'operation', target: AssignmentDeliveryTarget | null | undefined) {
    need(target, 'secretary_assignment_channel_unconfigured', 'Secretary has no Main notification target.');
    const node = await this.engine.client.request('serviceNodes.get', { serviceNodeId: target.serviceNodeId });
    need(node.serviceName === 'chat-bridge' && node.connected && node.synced && node.ready, 'secretary_assignment_channel_unavailable', 'The selected ChatBridge owner is unavailable.');
    return serviceTools(this.engine.client, target.serviceNodeId, [{ namespace: 'chat', interfaceVersion: chatInterfaceVersion }]).binding('chat.' + name);
  }

  async step(execution: Document<'secretary/execution'>): Promise<boolean> {
    await this.engine.verifyOwner();
    const value = execution.value;
    need(value.result !== null, 'secretary_assignment_result_invalid', 'An assignment needs its retained result.');
    const decision = assignmentDecision(value.result);
    const settled = async () => { if (decision.notification === 'voice') await this.voice.completeFallback(execution); return true; };
    const currentAssignment = await this.engine.store.named('secretary/assignment', assignmentName(value.assignmentSnapshot.assignmentId));
    const currentConfiguration = await this.engine.store.named('secretary/configuration', 'configuration');
    need(currentAssignment && currentConfiguration, 'secretary_assignment_delivery_unavailable', 'The current assignment and contact rules must be available.');
    const rules = effectiveRules(currentConfiguration.value.rules, currentAssignment.value.rules);
    let urgentVoiceFallback = false;
    if (decision.notification === 'voice') {
      const eligible = currentAssignment.value.enabled && value.effectiveRules.voice.enabled && rules.voice.enabled &&
        decision.urgency === 'critical' && contactWindowOpen(rules.voice, this.engine.now());
      const result = await this.voice.step(execution, decision, eligible);
      if (result === 'confirmed') return true;
      if (result === 'pending') return false;
      if (eligible && !rules.main.enabled) throw new IvyError('secretary_voice_fallback_unavailable', 'A failed Voice call has no enabled Main fallback.');
      urgentVoiceFallback = eligible;
    }
    const expiresAt = this.engine.now().getTime() + 8 * 86_400_000;
    let latest = await this.engine.store.read('secretary/execution', execution.pin.objectId);
    const save = async (handoff: Handoff) => {
      await this.engine.verifyOwner();
      latest = await this.engine.store.amend(latest, 'secretary/execution', { ...latest.value, delivery: handoff });
      return { value: handoff };
    };
    const legacy = this.engine.store.technicalNamed<Handoff>(key, value.executionId);
    let saved = latest.value.delivery ? { value: latest.value.delivery } : legacy ? await save(legacy.value) : null;
    if (!saved) saved = await save({ executionId: value.executionId, resultHash: hashJson(value.result), operationId: null,
      request: null, state: 'queued', reason: null, evidence: null, expiresAt: null });
    need(saved.value.executionId === value.executionId && saved.value.resultHash === hashJson(value.result), 'secretary_assignment_result_changed', 'The retained decision changed after delivery began.');
    if (saved.value.state === 'confirmed' || saved.value.state === 'suppressed') {
      this.engine.recoveryIssues.delete('delivery:' + value.executionId);
      return settled();
    }
    const reconcileOnly = saved.value.state === 'outcome_unknown' &&
      (!!saved.value.terminal || ['archived', 'settled'].includes(latest.value.phase));
    if (saved.value.state === 'failed' || reconcileOnly && !saved.value.evidence) {
      this.engine.issue('delivery:' + value.executionId, saved.value.reason ?? 'secretary_assignment_notice_outcome_unknown');
      return settled();
    }
    const now = this.engine.now();
    if ((saved.value.retryAt ?? 0) > now.getTime()) return false;
    const rule = rules.main;
    const suppressed = decision.notification === 'none' ? 'notification_none' : !currentAssignment.value.enabled ? 'assignment_disabled'
      : !rule.enabled ? 'main_disabled' : rank[decision.urgency] < rank[rule.minimumUrgency] ? 'below_minimum_urgency' : null;
    const minimumAge = currentAssignment.value.minimumNotificationAgeMinutes ?? this.minimumNotificationAgeMinutes;
    const deferred = !urgentVoiceFallback && now.getTime() < Date.parse(value.trigger.occurredAt) + minimumAge * 60_000 ? 'minimum_message_age'
      : !contactWindowOpen(rule, now) ? 'outside_contact_window' : null;
    if (!reconcileOnly && !suppressed && !deferred)
      await secretaryBrowserNotice(this.engine, hashJson([value.executionId, value.result]), decision.text);
    if (!saved.value.request) {
      if (suppressed) {
        await save({ ...saved.value, state: 'suppressed', reason: decision.notification === 'none' ? decision.reason : suppressed, expiresAt });
        return settled();
      }
      if (deferred) {
        if (saved.value.reason !== deferred) await save({ ...saved.value, reason: deferred });
        return false;
      }
      const target = await mainChat(this.engine.client), operationId = await newOperationId(this.engine.client);
      need(target.expectedBridge.callerPrincipalId === this.engine.settings.identity.principalId, 'secretary_assignment_channel_unavailable', 'Main must identify Secretary as the actual caller.');
      const request: Chat.AdmittedNotifyRequest = { action: 'notify', operationId, expectedBridge: target.expectedBridge, channel: target.channel,
        source: execution.pin, text: decision.text + `\n\nSecretary reference: ${execution.pin.objectId}` };
      saved = await save({ ...saved.value, operationId, target: { serviceNodeId: target.serviceNodeId, channel: target.channel }, request, state: 'dispatching', reason: null });
    }
    const request = saved.value.request!;
    const target = saved.value.target ?? await mainChat(this.engine.client).then(main => {
      need(main.expectedBridge.principalId === request.expectedBridge.principalId && main.expectedBridge.workspaceId === request.expectedBridge.workspaceId &&
        main.expectedBridge.callerPrincipalId === request.expectedBridge.callerPrincipalId, 'secretary_assignment_notice_mismatch', 'The retained handoff belongs to another Main or caller.');
      return { serviceNodeId: main.serviceNodeId, channel: request.channel };
    });
    need(saved.value.operationId === request.operationId && request.source.objectId === execution.pin.objectId && target && same(target.channel, request.channel),
      'secretary_assignment_notice_mismatch', 'The original handoff source, target or operation changed.');
    try {
      let reply: Chat.ReplyView | null = null;
      try {
        const operation = await callBound(this.engine.client, await this.bound('operation', target), { expectedBridge: request.expectedBridge, operationId: request.operationId }) as unknown as Chat.Operation;
        need(operation.request.action === 'notify' && operation.callerPrincipalId === this.engine.settings.identity.principalId && same(mainNotification(operation.request), mainNotification(request)),
          'secretary_assignment_notice_mismatch', 'The original Main handoff identity changed.');
        if (!same(operation.request, request)) {
          await save({ ...saved.value, request: operation.request, target: { serviceNodeId: target.serviceNodeId, channel: operation.request.channel } }); return false;
        }
        reply = await callBound(this.engine.client, await this.bound('notice', target), { operationId: request.operationId }) as unknown as Chat.ReplyView;
      }
      catch (error) {
        if (!(error instanceof IvyError && ['not_found', 'chat_publication_pending'].includes(error.code))) throw error;
        // An uncertain delivery may gain a later acknowledgement. Only read the
        // original outcome; absence must never cause another notification.
        if (reconcileOnly) return settled();
        const reason = suppressed ?? deferred;
        if (reason) {
          if (saved.value.reason !== reason) await save({ ...saved.value, reason });
          return false;
        }
        const result = await callBound(this.engine.client, await this.bound('notify', target), mainNotification(request) as unknown as Wire.Json, request.operationId) as unknown as Chat.Operation;
        need(result.callerPrincipalId === this.engine.settings.identity.principalId && result.request.action === 'notify' && same(mainNotification(result.request), mainNotification(request)), 'secretary_assignment_notice_mismatch', 'ChatBridge changed the original notification request.');
        if (!same(result.request, request) && result.request.action === 'notify') {
          await save({ ...saved.value, request: result.request, target: { serviceNodeId: target.serviceNodeId, channel: result.request.channel } }); return false;
        }
        if (result.phase === 'succeeded' && result.outcome?.action === 'notify') reply = result.outcome.reply;
        else if (result.phase === 'failed' && result.error?.outcome === 'not_executed') {
          const attempts = (saved.value.attempts ?? 0) + 1;
          const terminal = attempts >= maxDeliveryAttempts;
          await save({ ...saved.value, operationId: terminal ? request.operationId : null,
            request: terminal ? request : null, state: terminal ? 'failed' : 'queued', reason: result.error.code,
            attempts, retryAt: terminal ? 0 : now.getTime() + Math.min(300_000, 30_000 * 2 ** (attempts - 1)), expiresAt: terminal ? expiresAt : null });
          if (terminal) { this.engine.issue('delivery:' + value.executionId, result.error.code); return settled(); }
          return false;
        } else if (result.phase === 'failed') {
          const code = result.error?.code ?? 'secretary_assignment_notice_unresolved';
          await save({ ...saved.value, state: 'outcome_unknown', reason: code, expiresAt, terminal: true });
          this.engine.issue('delivery:' + value.executionId, code);
          return settled();
        } else { need(['accepted', 'applying'].includes(result.phase), 'secretary_assignment_notice_unresolved', 'Main did not accept the handoff.'); return false; }
      }
      need(reply && same(reply.data.channel, request.channel) && reply.data.origin.kind === 'native', 'secretary_assignment_notice_mismatch', 'Main did not retain the original reply.');
      const read = await this.engine.client.request('objects.read', reply.object);
      need(read.object.contractKey === 'chat-bridge/reply' && !read.object.effectivelyArchived && ['1.2.0', '1.3.0'].includes(read.revision.contractVersion) &&
        read.content.encoding === 'json' && same(read.content.value, reply.data) && hashJson(reply.data) === read.revision.contentHash,
        'secretary_assignment_notice_mismatch', 'The Main reply evidence is invalid.');
      const input = await this.engine.client.request('objects.read', { objectId: reply.data.origin.inputId });
      const handoff = input.content.encoding === 'json' ? input.content.value as Chat.Input : null;
      need(input.object.contractKey === 'chat-bridge/input' && !input.object.effectivelyArchived && ['1.2.0', '1.3.0'].includes(input.revision.contractVersion) &&
        handoff && hashJson(handoff) === input.revision.contentHash &&
        handoff.operationId === request.operationId && handoff.identity.senderPrincipalId === this.engine.settings.identity.principalId &&
        same(handoff.identity.channel, request.channel) && same(handoff.binding, reply.data.origin.binding) &&
        same(handoff.result, reply.data.origin.result), 'secretary_assignment_notice_mismatch', 'The Main reply belongs to a different handoff.');
      if (reply.data.state === 'outcome_unknown') {
        await save({ ...saved.value, state: 'outcome_unknown', reason: 'secretary_assignment_notice_outcome_unknown', evidence: reply.object, expiresAt, terminal: true, retryAt: now.getTime() + 30_000 });
        this.engine.issue('delivery:' + value.executionId, 'secretary_assignment_notice_outcome_unknown');
        return settled();
      }
      if (reply.data.state !== 'confirmed') return false;
      await save({ ...saved.value, state: 'confirmed', reason: null, evidence: reply.object, expiresAt });
      this.engine.recoveryIssues.delete('delivery:' + value.executionId);
      return settled();
    } catch (error) {
      const code = IvyError.from(error).code;
      if (saved.value.reason !== code)
        await save({ ...saved.value, reason: code });
      throw error;
    }
  }
}
