import { automaticNotificationsAllowed, hashJson, newOperationId, requireThat } from '../../../packages/sdk/src/node.js';
import type { Chat, Wire } from '../../../packages/sdk/src/node.js';
import type { ChatMain } from './chat-main.js';
import { ChatNotices } from './notices.js';

export const diagnosticNotificationDelayMs = 5 * 60_000;
export const diagnosticNotificationPollMs = 30_000;
export interface DiagnosticNoticeState {
  code: string; label: string; firstSeen: number; active: boolean; resolvedAt: number | null;
  alert: { request: Chat.NotifyRequest; queued: boolean; confirmed: boolean } | null;
  recovery: { request: Chat.NotifyRequest; queued: boolean; confirmed: boolean } | null;
  receiptOnly?: boolean;
}

const noticeKey = (entry: Pick<DiagnosticNoticeState, 'code' | 'label'>) => hashJson({ code: entry.code, label: entry.label });
function diagnosticNotice(value: Wire.Diagnostic): Pick<DiagnosticNoticeState, 'code' | 'label'> {
  const resource = value.resource && typeof value.resource === 'object' && !Array.isArray(value.resource) ? value.resource : {};
  const caller = typeof resource['callerPrincipalId'] === 'string' ? resource['callerPrincipalId'] : value.source.replace(/^service:/u, '');
  const target = typeof resource['serviceNodeId'] === 'string' && resource['serviceNodeId'] !== caller ? ' → ' + resource['serviceNodeId'] : '';
  return { code: value.code, label: caller + target };
}

/** The journal owns delivery; keep every unfinished original receipt when incidents are combined. */
function retainReceipts(state: Record<string, DiagnosticNoticeState>, entry: DiagnosticNoticeState, kept: Set<string>): void {
  for (const kind of ['alert', 'recovery'] as const) {
    const pending = entry[kind];
    if (!pending || pending.confirmed || kept.has(pending.request.operationId)) continue;
    state['receipt:' + hashJson(pending.request.operationId)] = {
      ...entry, active: false, receiptOnly: true, alert: null, recovery: null, [kind]: pending,
    };
  }
}

function groupedNoticeState(saved: Record<string, DiagnosticNoticeState>): Record<string, DiagnosticNoticeState> {
  const state: Record<string, DiagnosticNoticeState> = {}, groups = new Map<string, DiagnosticNoticeState[]>();
  for (const entry of Object.values(saved)) {
    if (entry.receiptOnly) { retainReceipts(state, entry, new Set()); continue; }
    const key = noticeKey(entry), entries = groups.get(key) ?? [];
    entries.push(entry); groups.set(key, entries);
  }
  for (const [key, entries] of groups) {
    const open = entries.filter(entry => entry.active || !entry.recovery?.queued);
    const pool = open.length ? open : entries;
    const prior = saved[key] && pool.includes(saved[key]!) ? saved[key]! : pool[0]!;
    const active = entries.some(entry => entry.active);
    const alert = prior.alert ?? pool.find(entry => entry.alert)?.alert ?? null;
    const recovery = active ? null : prior.recovery ?? pool.find(entry => entry.recovery)?.recovery ?? null;
    state[key] = { code: prior.code, label: prior.label, firstSeen: Math.min(...pool.map(entry => entry.firstSeen)),
      active, resolvedAt: active ? null : Math.max(...pool.map(entry => entry.resolvedAt ?? entry.firstSeen)), alert, recovery };
    const kept = new Set([alert?.request.operationId, recovery?.request.operationId].filter((id): id is string => !!id));
    for (const entry of entries) retainReceipts(state, entry, kept);
  }
  return state;
}

/** Shared operational notices use the existing Main queue and its original receipts. */
export class DiagnosticNotifications {
  constructor(private readonly main: ChatMain,
    private readonly settings: Pick<Chat.Settings, 'diagnosticNotifications' | 'language'> = {}, private readonly now = Date.now) {}
  private get scope(): string { return hashJson([this.main.admission.definition.principalId, this.main.admission.definition.workspaceId]); }
  async tick(signal: AbortSignal): Promise<void> {
    if (this.settings.diagnosticNotifications !== true) return;
    const current = new Map<string, Pick<DiagnosticNoticeState, 'code' | 'label'>>();
    let cursor: string | undefined;
    do {
      const page = await this.main.store.client.request('system.diagnostics', { status: 'unresolved', limit: 100, ...(cursor ? { cursor } : {}) }, { signal });
      for (const value of page.items)
        if (value.severity !== 'info' && value.code !== 'chat_diagnostics_unavailable') {
          const notice = diagnosticNotice(value);
          current.set(noticeKey(notice), notice);
        }
      cursor = page.nextCursor ?? undefined;
    } while (cursor);
    signal.throwIfAborted();
    // A failed or incomplete read cannot announce recovery or discard pending notices.
    const state = groupedNoticeState(this.main.store.diagnosticNoticeState(this.scope)), now = this.now();
    for (const [key, value] of current) {
      const prior = state[key];
      if (!prior || !prior.active && prior.recovery?.queued) {
        if (prior) retainReceipts(state, prior, new Set());
        state[key] = { ...value, firstSeen: now, active: true, resolvedAt: null, alert: null, recovery: null };
      } else if (!prior.recovery) { prior.active = true; prior.resolvedAt = null; }
    }
    for (const [key, entry] of Object.entries(state)) {
      if (entry.receiptOnly) continue;
      if (!current.has(key)) {
        if (!entry.alert) { delete state[key]; continue; }
        entry.active = false; entry.resolvedAt ??= now;
      }
      // Completed incidents retain deduplication across service and Hive restarts.
      if (entry.alert?.confirmed && entry.recovery?.confirmed && entry.resolvedAt !== null && now - entry.resolvedAt > 7 * 86_400_000) delete state[key];
    }
    this.main.store.saveDiagnosticNoticeState(this.scope, state);
    if (!await automaticNotificationsAllowed(this.main.store.client, new Date(now), { signal })) {
      // Do not accumulate overnight recoveries or resolved incidents for the morning.
      // Original admitted notices and their delivery receipts remain in Main's journal.
      for (const [key, entry] of Object.entries(state)) if (!entry.receiptOnly && !entry.active && !entry.recovery?.queued) delete state[key];
      this.main.store.saveDiagnosticNoticeState(this.scope, state); return;
    }
    for (const [key, entry] of Object.entries(state)) {
      signal.throwIfAborted();
      if (entry.receiptOnly) {
        const pending = entry.alert ?? entry.recovery!;
        if (pending.queued && !pending.confirmed) await this.review(pending, signal);
        this.main.store.saveDiagnosticNoticeState(this.scope, state);
        if (!pending.queued) await this.queue(pending, signal);
        if (pending.confirmed) delete state[key];
        this.main.store.saveDiagnosticNoticeState(this.scope, state); continue;
      }
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
    const storageWarning = entry.code === 'storage_pressure' && entry.label === 'hive';
    const text = this.settings.language === 'de'
      ? recovery ? `[Ivy] Erholung festgestellt: ${entry.label} meldet den Fehler ${entry.code} nicht mehr.`
        : storageWarning ? '[Ivy] Speicherwarnung: Der freie Speicher von Hive liegt unter der Betriebsreserve. Hive arbeitet weiter.'
        : `[Ivy] Anhaltender Fehler bei ${entry.label}: ${entry.code}. Der Dienst oder ein abhängiger Abruf kann blockiert sein; gespeicherte Daten können veraltet sein.`
      : recovery ? `[Ivy] Recovery observed: ${entry.label} no longer reports ${entry.code}.`
        : storageWarning ? '[Ivy] Storage warning: Hive has less free disk space than its operating reserve. Hive continues processing requests.'
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
