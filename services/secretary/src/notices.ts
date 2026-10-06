import { callBound, serviceTools, hashJson, IvyError, newOperationId, mainNotification, chatInterfaceVersion } from '../../../packages/sdk/src/node.js';
import type { Chat, Wire } from '../../../packages/sdk/src/node.js';
import { need, same } from './schema.js';
import type { Notice, NoticeTarget, Pin } from './schema.js';
import type { Document } from './store.js';
import { mutation } from './store.js';
import type { SecretaryEngine } from './engine.js';
import { ignored, quiet, suppression } from './policy.js';
import { forwardedVoiceOutcome, voicePrompt } from './assignment-voice.js';
import { secretaryBrowserNotice } from './browser-notice.js';
const chatVersions=new Set(['1.2.0', '1.3.0']);
interface NoticeRetry { failedOperationId: string; attempts: number; nextAt: number; expiresAt: number | null }
const retryKey = 'secretary/notice-retry';

export class SecretaryNotices {
  constructor(readonly engine: SecretaryEngine, readonly minimumMessageAgeMinutes = 5) {
    need(Number.isInteger(minimumMessageAgeMinutes) && minimumMessageAgeMinutes >= 0 && minimumMessageAgeMinutes <= 24 * 60,
      'invalid_arguments', 'Secretary notification delay must be a whole number of minutes between zero and one day.');
  }
  private async update(item: Document<'secretary/item'>, notice: Notice): Promise<void> {
    if (same(item.value.notice, notice)) return; await this.engine.verifyOwner();
    const operationId = item.value.decision?.operationId;
    need(operationId, 'secretary_notice_mismatch', 'A notice update needs its assessment operation identity.');
    await this.engine.store.write('secretary/item', { ...item.value, notice }, { object: item.pin }, mutation(operationId, item.pin, notice));
  }
  private async call(name: 'notice' | 'notify' | 'operation', target: NoticeTarget, args: Wire.Json, operationId?: string): Promise<Wire.Json> {
    await this.engine.verifyOwner(); const client = this.engine.client;
    const node = await client.request('serviceNodes.get', { serviceNodeId: target.serviceNodeId });
    need(node.serviceName === 'chat-bridge' && node.principalId === target.expectedBridge.principalId && node.connected && node.synced && node.ready,
      'secretary_notice_unavailable', 'The selected ChatBridge owner is not currently ready.');
    const bound = await serviceTools(client, target.serviceNodeId, [{ namespace: 'chat', interfaceVersion: chatInterfaceVersion }]).binding('chat.' + name);
    return callBound(client, bound, args, operationId);
  }
  private async evidence(reply: Chat.ReplyView, notice: Notice): Promise<void> {
    const client = this.engine.client, request = notice.request!;
    const read = await client.request('objects.read', reply.object);
    need(read.object.contractKey === 'chat-bridge/reply' && !read.object.effectivelyArchived && chatVersions.has(read.revision.contractVersion) && read.content.encoding === 'json',
      'secretary_notice_mismatch', 'The provider reply must be one active supported ChatBridge reply.');
    need(same(read.content.value, reply.data), 'secretary_notice_mismatch', 'The provider reply view differs from its saved delivery proof.');
    need(hashJson(reply.data) === read.revision.contentHash, 'secretary_notice_mismatch', 'The provider reply content hash differs from its saved delivery proof.');
    const value = reply.data;
    need(value.origin.kind === 'native', 'secretary_notice_mismatch', 'This reply was not written by the current Main.');
    need(same(value.channel, request.channel), 'secretary_notice_mismatch', 'This reply does not target the original Secretary notice channel.');
    const input = await client.request('objects.read', { objectId: value.origin.inputId });
    need(input.object.contractKey === 'chat-bridge/input' && !input.object.effectivelyArchived && chatVersions.has(input.revision.contractVersion) && input.content.encoding === 'json',
      'secretary_notice_mismatch', 'The Main reply must retain its exact hidden service handoff.');
    const handoff = input.content.value as Chat.Input;
    need(hashJson(handoff) === input.revision.contentHash && handoff.operationId === request.operationId &&
      handoff.identity.senderPrincipalId === this.engine.settings.identity.principalId && same(handoff.identity.channel, request.channel) &&
      same(handoff.binding, value.origin.binding) && same(handoff.result, value.origin.result),
      'secretary_notice_mismatch', 'The Main reply does not belong to this Secretary handoff.');
  }
  private async voice(item: Document<'secretary/item'>, notice: Notice): Promise<void> {
    const escalation = item.value.decision?.policy.voiceEscalation, voice = notice.voice;
    if (!voice || voice.state === 'confirmed') return;
    need(item.value.decision?.assessment.notification === 'voice' && item.value.decision.assessment.urgency === 'critical' && escalation,
      'secretary_voice_mismatch', 'Voice escalation requires the original critical decision and configured target.');
    try {
      await this.engine.verifyOwner();
      const node = await this.engine.client.request('serviceNodes.get', { serviceNodeId: escalation!.serviceNodeId });
      need(node.serviceName === 'phone-bridge' && node.connected && node.synced && node.ready, 'secretary_voice_unavailable', 'The configured Phone owner is not ready.');
      const bound = await serviceTools(this.engine.client, escalation!.serviceNodeId, [{ namespace: 'phone', interfaceVersion: '1.0.0' }]).binding('phone.request');
      need(bound.definitionHash === escalation!.requestDefinitionHash, 'tool_definition_changed', 'The configured Phone request definition changed.');
      let callId = voice.callId;
      if (!callId) {
        const result = await callBound(this.engine.client, bound, { operationId: voice.operationId, recipientId: escalation!.recipientId, route: 'voice', voicePrompt: voicePrompt(notice.text) }, voice.operationId) as Record<string, Wire.Json>;
        callId = typeof result['callId'] === 'string' ? result['callId'] : null;
        need(callId, 'secretary_voice_unresolved', 'PhoneBridge returned no original call identity.');
        if (result['operationId'] !== voice.operationId) {
          const operation = await serviceTools(this.engine.client, escalation!.serviceNodeId, [{ namespace: 'phone', interfaceVersion: '1.0.0' }]).binding('phone.operation');
          const forwarded = await callBound(this.engine.client, operation, { callId, operationId: voice.operationId }) as Record<string, Wire.Json> | null;
          need(forwarded, 'secretary_voice_unresolved', 'PhoneBridge returned no forwarded request outcome.');
          forwardedVoiceOutcome(forwarded, callId, voice.operationId, voicePrompt(notice.text));
        } else need(result['recipientId'] === escalation!.recipientId && result['principalId'] === this.engine.settings.identity.principalId,
          'secretary_voice_unresolved', 'Phone admission changed the original call.');
        await this.update(item, { ...notice, voice: { ...voice, state: 'queued', callId, errorCode: null }, updatedAt: this.engine.now().toISOString() });
        return;
      }
      const operation = await serviceTools(this.engine.client, escalation!.serviceNodeId, [{ namespace: 'phone', interfaceVersion: '1.0.0' }]).binding('phone.operation');
      const forwarded = await callBound(this.engine.client, operation, { callId, operationId: voice.operationId }) as Record<string, Wire.Json> | null;
      if (forwarded) {
        const outcome = forwardedVoiceOutcome(forwarded, callId, voice.operationId, voicePrompt(notice.text));
        if (outcome === 'pending') return;
        await this.update(item, { ...notice, voice: { ...voice, state: 'confirmed', callId, errorCode: null }, updatedAt: this.engine.now().toISOString() });
        return;
      }
      const dial = await callBound(this.engine.client, operation, { callId, method: 'call.dial' }) as Record<string, Wire.Json> | null;
      if (!dial || dial['phase'] === 'submitted') return;
      const dialReceipt = dial['receipt'] as Record<string, Wire.Json> | null, observed = dialReceipt?.['result'] as Record<string, Wire.Json> | null;
      need(dial['phase'] !== 'outcome_unknown' && dialReceipt?.['ok'] === true && observed?.['state'] === 'connected',
        'secretary_voice_not_connected', 'The original Phone call did not connect.');
      const prompted = await callBound(this.engine.client, operation, { callId, method: 'call.promptVoice', generation: 0 }) as Record<string, Wire.Json> | null;
      if (!prompted || prompted['phase'] === 'submitted') return;
      const promptReceipt = prompted['receipt'] as Record<string, Wire.Json> | null;
      const promptResult = promptReceipt?.['result'] as Record<string, Wire.Json> | null;
      need(prompted['phase'] !== 'outcome_unknown' && promptReceipt?.['ok'] === true && promptResult?.['state'] === 'sent',
        'secretary_voice_prompt_unresolved', 'The connected Voice prompt was not confirmed.');
      await this.update(item, { ...notice, voice: { ...voice, state: 'confirmed', callId, errorCode: null }, updatedAt: this.engine.now().toISOString() });
    } catch (error) {
      const code = IvyError.from(error).code;
      if (['revision_conflict', 'mutation_conflict'].includes(code)) throw error;
      await this.update(item, { ...notice, voice: { ...voice, state: 'outcome_unknown', errorCode: code }, updatedAt: this.engine.now().toISOString() });
    }
  }
  async step(id: string): Promise<void> {
    await this.engine.verifyOwner(); const item = await this.engine.item(id), decision = item.value.decision, notice = item.value.notice;
    if (!decision || !notice) return;
    need(notice.source.objectId === item.pin.objectId && decision.actor.length > 0 && decision.operationId.length > 0,
      'secretary_notice_mismatch', 'Dispatch requires the materialized assessment outcome.');
    const suppressed = suppression((await this.engine.source(item.value.sourceId)).value.definition, decision.assessment, decision.policy);
    if (suppressed) { need(notice.state === 'suppressed' && notice.reason === suppressed && !notice.request && !notice.evidence, 'secretary_notice_mismatch', 'A suppressed assessment cannot gain a dispatch.'); return; }
    need(notice.state !== 'suppressed', 'secretary_notice_mismatch', 'A required notice cannot be silently suppressed.');
    const now = this.engine.now(), at = now.toISOString(), isQuiet = quiet(this.engine.settings.policy, decision.assessment.urgency, now);
    const isTooYoung = decision.assessment.notification !== 'voice' && now.getTime() < Date.parse(item.value.message.occurredAt) + this.minimumMessageAgeMinutes * 60_000;
    const currentSource = this.engine.settings.sources.find(source => source.sourceId === item.value.sourceId);
    const authorized = currentSource && !ignored(currentSource, item.value.message);
    if (authorized && !isQuiet && !isTooYoung && notice.state !== 'confirmed')
      await secretaryBrowserNotice(this.engine, hashJson([id, decision.operationId]), decision.assessment.summary);
    const idempotency = mutation(id, decision.actor, decision.operationId, 'notice');
    const retry = this.engine.store.technicalNamed<NoticeRetry>(retryKey, id);
    if (notice.state === 'outcome_unknown' && notice.reason !== 'display_outcome_unknown') return;
    if (notice.request && retry?.value.failedOperationId === notice.request.operationId) {
      if (retry.value.nextAt > now.getTime()) return;
      if (retry.value.attempts >= 5) {
        await this.update(item, { ...notice, state: 'outcome_unknown', reason: 'chat_operation_failed', updatedAt: at });
        this.engine.issue('notice:' + id, 'chat_operation_failed'); return;
      }
      const source = { objectId: item.pin.objectId, revision: item.pin.revision + 1 };
      const request = { ...notice.request, operationId: await newOperationId(this.engine.client), source };
      await this.update(item, { ...notice, source, request, preparedRevision: source.revision, state: 'dispatching', reason: null, updatedAt: at });
      return;
    }
    if (!notice.request) {
      need(notice.state === 'queued' && !notice.target && notice.preparedRevision === null && !notice.evidence, 'secretary_notice_mismatch', 'Unprepared notices must remain queued.');
      const target = await this.engine.noticeTarget(), reason = !authorized ? 'authorization_changed' : isTooYoung ? 'minimum_message_age' : isQuiet ? 'silent_time' : !target ? 'channel_not_configured' : null;
      if (reason) { if (notice.reason !== reason) await this.update(item, { ...notice, reason, updatedAt: at }); return; }
      const source = { objectId: item.pin.objectId, revision: item.pin.revision + 1 };
      const request: Chat.AdmittedNotifyRequest = { action: 'notify', operationId: idempotency, expectedBridge: target!.expectedBridge, channel: target!.channel, source, text: notice.text + `\n\nSecretary reference: ${item.pin.objectId}` };
      await this.update(item, { ...notice, source, target: target!, request, preparedRevision: source.revision, state: 'dispatching', reason: null, updatedAt: at }); return;
    }
    need(notice.target && notice.preparedRevision !== null, 'secretary_notice_mismatch', 'A dispatched notice must retain its prepared target revision.');
    need(notice.request.action === 'notify' && same(notice.request.source, notice.source) &&
      [notice.text, notice.text + `\n\nSecretary reference: ${item.pin.objectId}`].includes(notice.request.text) &&
      same(notice.request.expectedBridge, notice.target.expectedBridge) && same(notice.request.channel, notice.target.channel) && notice.target.expectedBridge.callerPrincipalId === this.engine.settings.identity.principalId,
      'secretary_notice_mismatch', 'Recovery cannot change the original prepared recipient or notice arguments.');
    if (notice.state === 'confirmed') { this.engine.recoveryIssues.delete('notice:' + id); await this.voice(item, notice); return; }
    try {
      let reply: Chat.ReplyView | null = null;
      try {
        const operation = await this.call('operation', notice.target, { expectedBridge: notice.request.expectedBridge, operationId: notice.request.operationId } as Wire.Json) as Chat.Operation;
        need(operation.request.action === 'notify' && operation.callerPrincipalId === this.engine.settings.identity.principalId && same(mainNotification(operation.request), mainNotification(notice.request)),
          'secretary_notice_mismatch', 'The original Main handoff identity changed.');
        if (!same(operation.request, notice.request)) {
          await this.update(item, { ...notice, request: operation.request,
            target: { ...notice.target, expectedBridge: operation.request.expectedBridge, channel: operation.request.channel }, updatedAt: at }); return;
        }
        reply = await this.call('notice', notice.target, { operationId: notice.request.operationId } as Wire.Json) as Chat.ReplyView;
      }
      catch (error) {
        if (!(error instanceof IvyError && ['not_found', 'chat_publication_pending'].includes(error.code))) throw error;
        if (!authorized) { if (notice.reason !== 'authorization_changed') await this.update(item, { ...notice, reason: 'authorization_changed', updatedAt: at }); return; }
        if (isQuiet) { if (notice.reason !== 'silent_time') await this.update(item, { ...notice, reason: 'silent_time', updatedAt: at }); return; }
        const result = await this.call('notify', notice.target, mainNotification(notice.request) as unknown as Wire.Json, notice.request.operationId) as Chat.Operation;
        need(result.callerPrincipalId === this.engine.settings.identity.principalId && result.request.action === 'notify' && same(mainNotification(result.request), mainNotification(notice.request)), 'secretary_notice_unresolved', 'The original notification action changed identity.');
        if (!same(result.request, notice.request) && result.request.action === 'notify') {
          await this.update(item, { ...notice, request: result.request,
            target: { ...notice.target, expectedBridge: result.request.expectedBridge, channel: result.request.channel }, updatedAt: at }); return;
        }
        if (result.phase === 'succeeded' && result.outcome?.action === 'notify') reply = result.outcome.reply;
        else if (result.phase === 'failed' && result.error?.outcome === 'not_executed') {
          const attempts = (retry?.value.attempts ?? 0) + 1;
          const value: NoticeRetry = { failedOperationId: notice.request.operationId, attempts,
            nextAt: now.getTime() + Math.min(300_000, 30_000 * 2 ** (attempts - 1)), expiresAt: attempts >= 5 ? now.getTime() + 8 * 86_400_000 : null };
          if (retry) this.engine.store.technicalAmend(retryKey, retry, value);
          else this.engine.store.technicalCreate(retryKey, id, value);
          await this.update(item, { ...notice, state: attempts >= 5 ? 'outcome_unknown' : 'dispatching', reason: result.error.code, updatedAt: at });
          if (attempts >= 5) this.engine.issue('notice:' + id, result.error.code);
          return;
        }
        else if (result.phase === 'failed') {
          const code = result.error?.code ?? 'chat_operation_failed';
          await this.update(item, { ...notice, state: 'outcome_unknown', reason: code, updatedAt: at });
          this.engine.issue('notice:' + id, code); return;
        }
        else {
          need(['accepted', 'applying'].includes(result.phase), 'secretary_notice_unresolved', 'The original notification action cannot be completed by Main.');
          if (notice.state !== 'dispatching' || notice.reason !== 'awaiting_main')
            await this.update(item, { ...notice, state: 'dispatching', reason: 'awaiting_main', updatedAt: at });
          return;
        }
      }
      need(reply, 'secretary_notice_unresolved', 'The original notification action has no Main reply.');
      await this.evidence(reply, notice);
      const state = reply.data.state === 'confirmed' ? 'confirmed' : reply.data.state === 'outcome_unknown' ? 'outcome_unknown' : 'dispatching';
      const reason = state === 'confirmed' ? null : state === 'outcome_unknown' ? 'display_outcome_unknown' : 'awaiting_display';
      if (notice.state !== state || notice.reason !== reason || !same(notice.evidence, reply.object)) await this.update(item, { ...notice, state, reason, evidence: reply.object, updatedAt: at });
      if (state === 'outcome_unknown') this.engine.issue('notice:' + id, 'secretary_notice_outcome_unknown');
      if (state === 'confirmed') {
        this.engine.recoveryIssues.delete('notice:' + id);
        if (retry) this.engine.store.technicalDelete(retryKey, retry.pin.objectId);
      }
    } catch (error) {
      const issue = IvyError.from(error), code = issue.code;
      if (['revision_conflict', 'mutation_conflict'].includes(code)) throw error;
      if (['secretary_notice_mismatch', 'secretary_evidence_conflict', 'tool_definition_changed'].includes(code)) {
        await this.update(item, { ...notice, state: 'outcome_unknown', reason: code, updatedAt: at });
        this.engine.issue('notice:' + id, code); return;
      }
      if (issue.outcome === 'not_executed') {
        const attempts = (retry?.value.attempts ?? 0) + 1;
        const value: NoticeRetry = { failedOperationId: notice.request.operationId, attempts,
          nextAt: now.getTime() + Math.min(300_000, 30_000 * 2 ** (attempts - 1)), expiresAt: attempts >= 5 ? now.getTime() + 8 * 86_400_000 : null };
        if (retry) this.engine.store.technicalAmend(retryKey, retry, value);
        else this.engine.store.technicalCreate(retryKey, id, value);
        await this.update(item, { ...notice, state: attempts >= 5 ? 'outcome_unknown' : 'dispatching', reason: code, updatedAt: at });
        if (attempts >= 5) this.engine.issue('notice:' + id, code);
        return;
      }
      if (notice.reason !== code) await this.update(item, { ...notice, state: 'dispatching', reason: code, updatedAt: at });
    }
  }
}
