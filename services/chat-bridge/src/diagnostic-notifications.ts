import { hashJson, newOperationId, requireThat } from '../../../packages/sdk/src/node.js';
import type { Chat, Wire } from '../../../packages/sdk/src/node.js';
import type { ChatMain } from './chat-main.js';
import { ChatNotices } from './notices.js';

export const diagnosticNotificationDelayMs = 60_000;
export const diagnosticNotificationPollMs = 30_000;
export interface DiagnosticNoticeState {
  code: string; label: string; firstSeen: number; active: boolean; resolvedAt: number | null;
  alert: { request: Chat.NotifyRequest; queued: boolean; confirmed: boolean } | null;
  recovery: { request: Chat.NotifyRequest; queued: boolean; confirmed: boolean } | null;
}

/** Shared operational notices use the existing Main queue and its original receipts. */
export class DiagnosticNotifications {
  constructor(private readonly main: ChatMain, private readonly language = 'en', private readonly now = Date.now) {}
  private get scope(): string { return hashJson([this.main.admission.definition.principalId, this.main.admission.definition.workspaceId]); }
  async tick(signal: AbortSignal): Promise<void> {
    const current = new Map<string, Wire.Diagnostic>();
    let cursor: string | undefined;
    do {
      const page = await this.main.store.client.request('system.diagnostics', { status: 'unresolved', limit: 100, ...(cursor ? { cursor } : {}) }, { signal });
      for (const value of page.items)
        if (value.severity !== 'info' && value.code !== 'chat_diagnostics_unavailable')
          current.set(hashJson({ code: value.code, source: value.source, resource: value.resource }), value);
      cursor = page.nextCursor ?? undefined;
    } while (cursor);
    signal.throwIfAborted();
    // A failed or incomplete read cannot announce recovery or discard pending notices.
    const state = this.main.store.diagnosticNoticeState(this.scope), now = this.now();
    for (const [key, value] of current) {
      const prior = state[key];
      if (!prior || !prior.active && prior.recovery?.queued) {
        const resource = value.resource && typeof value.resource === 'object' && !Array.isArray(value.resource) ? value.resource : {};
        const caller = typeof resource['callerPrincipalId'] === 'string' ? resource['callerPrincipalId'] : value.source.replace(/^service:/u, '');
        const target = typeof resource['serviceNodeId'] === 'string' && resource['serviceNodeId'] !== caller ? ' → ' + resource['serviceNodeId'] : '';
        state[key] = { code: value.code, label: caller + target, firstSeen: now, active: true, resolvedAt: null, alert: null, recovery: null };
      } else if (!prior.recovery) { prior.active = true; prior.resolvedAt = null; }
    }
    for (const [key, entry] of Object.entries(state)) {
      if (!current.has(key)) {
        if (!entry.alert) { delete state[key]; continue; }
        entry.active = false; entry.resolvedAt ??= now;
      }
      // Completed incidents retain deduplication across service and Hive restarts.
      if (entry.recovery?.confirmed && entry.resolvedAt !== null && now - entry.resolvedAt > 7 * 86_400_000) delete state[key];
    }
    this.main.store.saveDiagnosticNoticeState(this.scope, state);
    for (const entry of Object.values(state)) {
      signal.throwIfAborted();
      if (!entry.alert && entry.active && now - entry.firstSeen >= diagnosticNotificationDelayMs)
        entry.alert = { request: await this.request(entry, false), queued: false, confirmed: false };
      if (entry.alert?.queued && !entry.alert.confirmed) {
        await this.review(entry.alert, signal);
        this.main.store.saveDiagnosticNoticeState(this.scope, state);
      }
      if (entry.alert && !entry.alert.queued) {
        this.main.store.saveDiagnosticNoticeState(this.scope, state);
        await this.queue(entry.alert, signal);
        this.main.store.saveDiagnosticNoticeState(this.scope, state);
      }
      if (!entry.active && entry.alert?.queued && !entry.recovery && entry.resolvedAt !== null &&
        now - entry.resolvedAt >= diagnosticNotificationDelayMs)
        entry.recovery = { request: await this.request(entry, true), queued: false, confirmed: false };
      if (entry.recovery?.queued && !entry.recovery.confirmed) {
        await this.review(entry.recovery, signal);
        this.main.store.saveDiagnosticNoticeState(this.scope, state);
      }
      if (entry.recovery && !entry.recovery.queued) {
        this.main.store.saveDiagnosticNoticeState(this.scope, state);
        await this.queue(entry.recovery, signal);
        this.main.store.saveDiagnosticNoticeState(this.scope, state);
      }
    }
  }
  private async request(entry: DiagnosticNoticeState, recovery: boolean): Promise<Chat.NotifyRequest> {
    const main = await this.main.find();
    requireThat(main, 'chat_main_missing', 'Operational notifications need the configured Main conversation.');
    const text = this.language === 'de'
      ? recovery ? `[Ivy] Erholung festgestellt: ${entry.label} meldet den Fehler ${entry.code} nicht mehr.`
        : `[Ivy] Anhaltender Fehler bei ${entry.label}: ${entry.code}. Der Dienst oder ein abhängiger Abruf kann blockiert sein; gespeicherte Daten können veraltet sein.`
      : recovery ? `[Ivy] Recovery observed: ${entry.label} no longer reports ${entry.code}.`
        : `[Ivy] Persistent failure at ${entry.label}: ${entry.code}. The service or a dependent read may be blocked; retained data may be outdated.`;
    return { action: 'notify', operationId: await newOperationId(this.main.store.client), source: main.value.conversation, text };
  }
  private async queue(pending: NonNullable<DiagnosticNoticeState['alert']>, signal: AbortSignal): Promise<void> {
    signal.throwIfAborted();
    const caller = this.main.admission.definition.principalId;
    const operation = await new ChatNotices(this.main).notify(caller, pending.request);
    requireThat(operation.phase !== 'failed', 'chat_operational_notice_failed', 'The original operational notice failed before delivery.');
    pending.queued = true;
  }
  private async review(pending: NonNullable<DiagnosticNoticeState['alert']>, signal: AbortSignal): Promise<void> {
    signal.throwIfAborted();
    const caller = this.main.admission.definition.principalId, original = await this.main.operations.find(caller, pending.request.operationId);
    if (!original) return;
    if (original.value.phase === 'failed') {
      requireThat(original.value.error?.outcome === 'not_executed', 'chat_operational_notice_failed', 'Reconcile the original operational notice before retrying delivery.');
      await this.main.native.ready();
      pending.request = { ...pending.request, operationId: await newOperationId(this.main.store.client) };
      pending.queued = false;
    } else if (original.value.phase === 'succeeded') {
      const reply = await new ChatNotices(this.main).notice(caller, { operationId: pending.request.operationId });
      pending.confirmed = reply.data.state === 'confirmed';
    }
  }
}
