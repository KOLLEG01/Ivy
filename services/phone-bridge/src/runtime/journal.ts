import { DatabaseSync, type StatementSync } from 'node:sqlite';
import { randomUUID } from 'node:crypto';
import { mkdirSync, openSync, closeSync, realpathSync } from 'node:fs';
import { isAbsolute, join } from 'node:path';
import { canonical, digest } from '../../../../packages/sdk/src/node.js';
import { IvyError, requireThat } from '../../../../packages/sdk/src/node.js';
import { phoneValidator } from './validation.js';
import { validateShared } from '../../../../packages/sdk/src/node.js';
import { phoneFrameBytes, validatePhone } from './native.js';
import type { PhoneIntent, PhoneReceiptHooks, PhoneReply } from './native.js';
import contract from '../../../../specs/schemas/phone-journal.schema.json' with { type: 'json' };
import { phoneCallReservation, validatePhoneAdmission } from './admission.js';
import type { PhoneCall, PhoneCallAdmission } from './admission.js';
import archiveContract from '../../../../specs/schemas/phone-voice-archive.schema.json' with { type: 'json' };
import appToolsContract from '../../../../specs/schemas/codex-app-tools.schema.json' with { type: 'json' };
import type { PhoneVoiceArchivePlan, PhoneVoiceArchiveResult, PhoneVoiceArchiveSettings } from './desktop-voice-archive.js';
import { ReceiptArchive } from '../../../../packages/sdk/src/receipt-archive.js';
import type { PhoneVoiceSelection } from './voice-selection.js';

export type PhoneJournalIntent = PhoneIntent |
  (Omit<PhoneIntent, 'method'> & { method: 'call.archiveVoice'; archivePlan: PhoneVoiceArchivePlan }) |
  (Omit<PhoneIntent, 'method'> & { method: 'call.createVoice'; model: string; reasoningEffort: string }) |
  (Omit<PhoneIntent, 'method'> & { method: 'call.bindVoice' | 'call.stopVoice'; threadId: string }) |
  (Omit<PhoneIntent, 'method'> & { method: 'call.promptVoice' | 'call.forwardVoice'; threadId: string; prompt: string; model?: string; reasoningEffort?: string }) |
  (Omit<PhoneIntent, 'method'> & { method: 'call.selectVoice'; threadId: string; prompt: string; model: string; reasoningEffort: string; commandSequence?: number }) |
  (Omit<PhoneIntent, 'method'> & { method: 'call.restartVoice'; voiceGeneration: number; model: string; reasoningEffort: string });

export interface PhoneOperation {
  schemaVersion: 1; intent: PhoneJournalIntent; phase: 'submitted' | 'result' | 'outcome_unknown';
  receipt: Pick<PhoneReply, 'ok' | 'result' | 'error'> | null; createdAt: string; updatedAt: string;
  archiveResolution?: { state: 'observed_archived'; checkedAt: string };
}
export interface PhoneJournalLimits { maxOperations: number; maxBytes: number; maxEpochs: number }
/** Historical deletion evidence remains readable; current maintenance only archives. */
export interface RetiredVoiceTask {
  threadId: string; principalId: string; successorId: string; callId: string;
  phase: 'pending_archive' | 'archiving' | 'archived' | 'deleting' | 'deleted' | 'retained';
  archiveOperationId: string | null; deleteOperationId: string | null;
  archivedAt: string | null; deleteAfter: string | null;
}
export const phoneOutcomeReservation = phoneFrameBytes + 2048;
export const phoneArchivePlanBytes = 32 * 1024;
export interface PhoneCallTarget {
  callId: string; desktop: Record<string, unknown>; audio: Record<string, unknown>; voiceInput: Record<string, unknown>;
  voiceArchive: PhoneVoiceArchiveSettings | null;
}
export interface PhoneCodexTaskCache {
  fingerprint: string; threadId: string | null; creationOperationId: string | null;
  preparationOperationId?: string; preparationTurnId?: string;
}
export interface PhoneVoiceText { role: 'user' | 'assistant'; text: string }
export interface PhoneConversation {
  fingerprint: string; partyKey: string; threadId: string; callId: string; generation: number;
  items: PhoneVoiceText[];
}
const validateContract = phoneValidator({ ...appToolsContract.$defs, ...archiveContract.$defs, ...contract.$defs });
function validate(name: string, value: unknown): void {
  validateContract(name, value, phoneOutcomeReservation);
}
function intentShape(intent: PhoneJournalIntent): void {
  validate('Intent', intent);
  requireThat(intent.voiceGeneration === undefined || ['call.stopVoice', 'call.realtime.prepare', 'call.realtime.answer', 'call.realtime.stop', 'call.desktop.pauseVoice', 'call.desktop.resumeVoice', 'call.archiveVoice', 'call.createVoice', 'call.bindVoice', 'call.promptVoice', 'call.forwardVoice', 'call.restartVoice'].includes(intent.method),
    'invalid_arguments', 'Only Voice generation operations may carry a generation.');
  requireThat(intent.method.startsWith('call.') === (intent.callId !== null), 'invalid_arguments', 'Phone call commands require their original call identity.');
  if (intent.method === 'call.archiveVoice') requireThat(intent.requestHash === digest(canonical(intent.archivePlan, phoneArchivePlanBytes)),
    'phone_archive_plan_changed', 'Archive intent must retain its exact original bounded plan.');
  if (intent.method === 'call.archiveVoice' && intent.archivePlan.threadId !== undefined)
    requireThat(intent.archivePlan.candidates.length === 1 && intent.archivePlan.candidates[0]!.threadId === intent.archivePlan.threadId,
      'phone_archive_scope_conflict', 'A bound Voice archive plan must contain exactly its original task subtree.');
  if (intent.method === 'call.selectVoice') requireThat(intent.requestHash === digest(canonical({
    threadId: intent.threadId, prompt: intent.prompt, model: intent.model,
    reasoningEffort: intent.reasoningEffort, ...(intent.commandSequence === undefined ? {} : { commandSequence: intent.commandSequence }),
  })), 'phone_voice_selection_changed', 'Voice selection intent must retain its exact original arguments.');
  if (intent.method === 'call.restartVoice') requireThat(intent.requestHash === digest(canonical({
    voiceGeneration: intent.voiceGeneration, model: intent.model, reasoningEffort: intent.reasoningEffort,
  })), 'phone_voice_restart_changed', 'Voice restart must retain its original generation and selection.');
}

/** Phone's local effect boundary. Only explicit non-secret targets; no credential arguments,
 * retries or time-based eviction. */
export class PhoneJournal {
  readonly dataRoot: string;
  private readonly db: DatabaseSync;
  private readonly statements = new Map<string, StatementSync>();
  private readonly archive: ReceiptArchive;
  private closed = false;
  readonly hooks: PhoneReceiptHooks = {
    beforeSend: async intent => { this.submit(intent); },
    beforeResolve: async (intent, reply) => { this.finish(intent, reply); },
  };
  constructor(root: string, owner: { hostId: string; serviceNodeId: string },
    readonly limits: PhoneJournalLimits = { maxOperations: 4096, maxBytes: 512 * 1024 * 1024, maxEpochs: 4096 }, readonly maxConcurrentCalls = 1) {
    requireThat(Number.isInteger(maxConcurrentCalls) && maxConcurrentCalls >= 1 && maxConcurrentCalls <= 32, 'invalid_arguments', 'Call concurrency must be between 1 and 32.');
    requireThat(isAbsolute(root), 'invalid_arguments', 'Phone journal requires an absolute owned data directory.');
    validateShared('Identifier', owner.hostId); validateShared('Identifier', owner.serviceNodeId);
    requireThat(Number.isSafeInteger(limits.maxOperations) && limits.maxOperations >= 1 && limits.maxOperations <= 100000 &&
      Number.isSafeInteger(limits.maxBytes) && limits.maxBytes >= phoneOutcomeReservation && limits.maxBytes <= 8 * 1024 ** 3 &&
      Number.isSafeInteger(limits.maxEpochs) && limits.maxEpochs >= 1 && limits.maxEpochs <= 100000,
    'invalid_arguments', 'Phone journal limits are outside their bounded range.');
    mkdirSync(root, { recursive: true, mode: 0o700 });
    this.dataRoot = realpathSync(root);
    const filename = join(root, 'phone-commands.sqlite');
    try { closeSync(openSync(filename, 'wx', 0o600)); } catch (error) { if ((error as NodeJS.ErrnoException).code !== 'EEXIST') throw error; }
    const db = new DatabaseSync(filename); this.db = db;
    try {
      db.exec('PRAGMA busy_timeout=100;');
      try { db.exec('PRAGMA locking_mode=EXCLUSIVE; PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; BEGIN EXCLUSIVE; PRAGMA user_version; COMMIT;'); }
      catch (error) {
        if ([5, 6].includes(Number((error as { errcode?: number }).errcode) & 255)) throw new IvyError('phone_owner_already_running', 'Another Phone owner holds the command journal.');
        throw error;
      }
      const format = Number(db.prepare('PRAGMA user_version').get()!['user_version']);
      requireThat(format === 0 || format === 7, 'unsupported_storage', 'Phone journal requires format 7.');
      this.transaction(() => {
        db.exec(`CREATE TABLE IF NOT EXISTS meta(key TEXT PRIMARY KEY,value TEXT NOT NULL) STRICT;
          CREATE TABLE IF NOT EXISTS epochs(epoch TEXT PRIMARY KEY) STRICT;
          CREATE TABLE IF NOT EXISTS commands(operation_id TEXT PRIMARY KEY,epoch TEXT NOT NULL,call_id TEXT,method TEXT NOT NULL,phase TEXT NOT NULL,value TEXT NOT NULL) STRICT;
          CREATE INDEX IF NOT EXISTS commands_by_epoch ON commands(epoch,phase);
          CREATE INDEX IF NOT EXISTS commands_by_call ON commands(call_id,method);
          CREATE TABLE IF NOT EXISTS calls(call_id TEXT PRIMARY KEY,operation_id TEXT NOT NULL UNIQUE,value TEXT NOT NULL) STRICT;
          CREATE TABLE IF NOT EXISTS targets(call_id TEXT PRIMARY KEY,value TEXT NOT NULL) STRICT;
          CREATE TABLE IF NOT EXISTS active_calls(call_id TEXT PRIMARY KEY) STRICT;
          PRAGMA user_version=7;`);
        const identity = canonical(owner), prior = this.meta('owner');
        requireThat(prior === null || prior === identity, 'target_conflict', 'Phone data belongs to another host or Service Node.');
        this.setMeta('owner', identity);
        this.setMeta('count', String(db.prepare('SELECT count(*) AS count FROM commands').get()!['count']));
        this.setMeta('epochs', String(db.prepare('SELECT count(*) AS count FROM epochs').get()!['count']));
        this.setMeta('calls', String(db.prepare('SELECT count(*) AS count FROM calls').get()!['count']));
        this.setMeta('targets', String(db.prepare('SELECT count(*) AS count FROM targets').get()!['count']));
      });
      this.archive = new ReceiptArchive(db);
    } catch (error) { db.close(); throw error; }
  }
  private statement(sql: string): StatementSync {
    let statement = this.statements.get(sql);
    if (!statement) { statement = this.db.prepare(sql); this.statements.set(sql, statement); }
    return statement;
  }
  close(): void { if (!this.closed) { this.closed = true; this.statements.clear(); this.db.close(); } }
  private transaction<T>(work: () => T): T {
    this.db.exec('BEGIN IMMEDIATE');
    try { const result = work(); this.db.exec('COMMIT'); return result; }
    catch (error) { try { this.db.exec('ROLLBACK'); } catch { /* SQLite may already have rolled back a full transaction. */ } throw error; }
  }
  private meta(key: string): string | null { const row = this.statement('SELECT value FROM meta WHERE key=?').get(key); return row ? String(row['value']) : null; }
  private setMeta(key: string, value: string): void { this.statement('INSERT INTO meta VALUES (?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value').run(key, value); }
  rememberedVoiceReasoning(): PhoneVoiceSelection['reasoningEffort'] | null {
    const value = this.meta('voiceReasoning');
    requireThat(value === null || ['low', 'medium', 'high', 'xhigh', 'max', 'ultra'].includes(value),
      'phone_storage_invalid', 'Remembered Voice reasoning is invalid.');
    return value as PhoneVoiceSelection['reasoningEffort'] | null;
  }
  rememberVoiceReasoning(value: PhoneVoiceSelection['reasoningEffort']): void {
    requireThat(['low', 'medium', 'high', 'xhigh', 'max', 'ultra'].includes(value),
      'invalid_arguments', 'Unsupported Voice reasoning.');
    this.setMeta('voiceReasoning', value);
  }
  voiceConversation(fingerprint: string): PhoneConversation | null {
    const value = this.meta(`voiceConversation:${fingerprint}`);
    if (!value) return null;
    const context = JSON.parse(value) as PhoneConversation;
    this.validateConversation(context);
    requireThat(context.fingerprint === fingerprint, 'phone_storage_invalid', 'Voice context belongs to another runtime.');
    return context;
  }
  retainVoiceConversation(context: PhoneConversation): void {
    this.validateConversation(context);
    const stop = this.callCommand(context.callId, 'call.stopVoice', context.generation);
    requireThat(stop?.phase === 'result' && stop.receipt?.ok &&
      stop.intent.method === 'call.stopVoice' && stop.intent.threadId === context.threadId,
      'phone_voice_stop_unknown', 'Only a positively stopped Voice session may be continued.');
    this.setMeta(`voiceConversation:${context.fingerprint}`, canonical(context, 128 * 1024));
  }
  forgetVoiceConversation(fingerprint: string): void {
    this.statement('DELETE FROM meta WHERE key=?').run(`voiceConversation:${fingerprint}`);
  }
  private validateConversation(context: PhoneConversation): void {
    validate('Uuid', context.threadId); validate('Uuid', context.callId);
    requireThat(/^sha256:[0-9a-f]{64}$/.test(context.fingerprint) && /^sha256:[0-9a-f]{64}$/.test(context.partyKey) &&
      Number.isInteger(context.generation) && context.generation >= 0 && context.generation <= 128 &&
      Array.isArray(context.items) && context.items.length <= 126 && context.items.every(item =>
        item && ['user', 'assistant'].includes(item.role) && typeof item.text === 'string') &&
      context.items.reduce((sum, item) => sum + Buffer.byteLength(item.text), 0) <= 24000,
      'phone_storage_invalid', 'Voice continuation exceeds its bounded transcript context.');
  }
  codexTaskCache(principalId: string, fingerprint: string): PhoneCodexTaskCache | null {
    validateShared('Identifier', principalId);
    const value = this.meta(`codexVoice:${principalId}:${fingerprint}`);
    if (!value) return null;
    const cache = JSON.parse(value) as PhoneCodexTaskCache;
    requireThat(cache.fingerprint === fingerprint && (cache.threadId === null || typeof cache.threadId === 'string') &&
      (cache.creationOperationId === null || typeof cache.creationOperationId === 'string'),
    'phone_storage_invalid', 'Cached Codex Voice task has invalid ownership.');
    if (cache.threadId) validate('Uuid', cache.threadId);
    if (cache.creationOperationId) validate('Uuid', cache.creationOperationId);
    if (cache.preparationOperationId) validate('Uuid', cache.preparationOperationId);
    if (cache.preparationTurnId) validate('Uuid', cache.preparationTurnId);
    return cache;
  }
  retainCodexTask(principalId: string, cache: PhoneCodexTaskCache): void {
    validateShared('Identifier', principalId);
    if (cache.threadId) validate('Uuid', cache.threadId);
    if (cache.creationOperationId) validate('Uuid', cache.creationOperationId);
    if (cache.preparationOperationId) validate('Uuid', cache.preparationOperationId);
    if (cache.preparationTurnId) validate('Uuid', cache.preparationTurnId);
    requireThat(/^sha256:[0-9a-f]{64}$/.test(cache.fingerprint) && !!cache.threadId !== !!cache.creationOperationId,
      'invalid_arguments', 'Cached Voice task must identify either a created task or its original pending creation.');
    this.setMeta(`codexVoice:${principalId}:${cache.fingerprint}`, canonical(cache));
  }
  usedCodexVoiceTask(threadId: string): boolean {
    validate('Uuid', threadId);
    return !!this.statement(`SELECT 1 FROM commands WHERE method='call.promptVoice'
      AND json_extract(value,'$.intent.threadId')=?
      UNION ALL SELECT 1 FROM receipt_archive WHERE kind='command'
      AND json_extract(value,'$.intent.method')='call.promptVoice'
      AND json_extract(value,'$.intent.threadId')=? LIMIT 1`).get(threadId, threadId);
  }
  get epoch(): string | null { return this.meta('epoch') || null; }
  get epochOperations(): number { return Number(this.meta('epochOperations') ?? 0); }
  status(): { operations: number; calls: number; reservedBytes: number; epochs: number } {
    const operations = Number(this.meta('count')), calls = Number(this.meta('calls'));
    return { operations, calls, reservedBytes: (operations + Number(this.meta('targets'))) * phoneOutcomeReservation + calls * phoneCallReservation, epochs: Number(this.meta('epochs')) };
  }
  archiveStatus(): { records: number; bytes: number } { return this.archive.status(); }
  private compact(): void {
    this.transaction(() => {
      for (const row of this.statement(`SELECT call_id,value,operation_id FROM calls WHERE call_id NOT IN (SELECT call_id FROM active_calls) AND NOT EXISTS
        (SELECT 1 FROM commands WHERE commands.call_id=calls.call_id AND
          (phase='submitted' AND epoch=? OR method='call.archiveVoice' AND
            (phase!='result' OR json_extract(value,'$.receipt.result.state') IS NOT 'archived') AND
            json_extract(value,'$.archiveResolution.state') IS NOT 'observed_archived')) LIMIT 32`).all(this.epoch ?? '')) {
        const id = String(row['call_id']);
        this.archive.retain('call', id, String(row['value']));
        this.archive.retain('call-operation', String(row['operation_id']), id);
        for (const command of this.statement('SELECT operation_id,method,value FROM commands WHERE call_id=? ORDER BY rowid').all(id)) {
          this.archive.retain('command', String(command['operation_id']), String(command['value']));
          const archivedIntent = (JSON.parse(String(command['value'])) as PhoneOperation).intent;
          const generation = archivedIntent.voiceGeneration ?? 0;
          const key = canonical(generation === 0 ? [id, command['method']] : [id, command['method'], generation]);
          if (this.archive.read('call-command', key) === null) this.archive.retain('call-command', key, String(command['operation_id']));
          if (command['method'] === 'call.command.feedback' && archivedIntent.commandSequence)
            this.archive.retain('command-feedback', canonical([id, archivedIntent.commandSequence]), String(command['operation_id']));
          if (command['method'] === 'call.selectVoice' && archivedIntent.commandSequence)
            this.archive.retain('voice-selection', canonical([id, archivedIntent.commandSequence]), String(command['operation_id']));
        }
        const target = this.statement('SELECT value FROM targets WHERE call_id=?').get(id);
        if (target) this.archive.retain('target', id, String(target['value']));
        this.statement('DELETE FROM commands WHERE call_id=?').run(id);
        this.statement('DELETE FROM targets WHERE call_id=?').run(id);
        this.statement('DELETE FROM calls WHERE call_id=?').run(id);
      }
      // Unknown outcomes stay immutable in ReceiptArchive, but no longer consume
      // the bounded active journal after their native epoch has retired.
      for (const row of this.statement("SELECT operation_id,value FROM commands WHERE call_id IS NULL AND phase IN ('result','outcome_unknown') LIMIT 128").all()) {
        this.archive.retain('command', String(row['operation_id']), String(row['value']));
        this.statement('DELETE FROM commands WHERE operation_id=?').run(row['operation_id']!);
      }
      for (const row of this.statement('SELECT epoch FROM epochs WHERE epoch!=? LIMIT 128').all(this.epoch ?? '')) {
        this.archive.retain('epoch', String(row['epoch']), String(row['epoch']));
        this.statement('DELETE FROM epochs WHERE epoch=?').run(row['epoch']!);
      }
      for (const [key, table] of [['count', 'commands'], ['epochs', 'epochs'], ['calls', 'calls'], ['targets', 'targets']] as const)
        this.setMeta(key, String(this.statement(`SELECT count(*) AS count FROM ${table}`).get()!['count']));
    });
  }
  target(callId: string): PhoneCallTarget | null {
    validate('Uuid', callId);
    const row = this.statement('SELECT value FROM targets WHERE call_id=?').get(callId);
    const encoded = row ? String(row['value']) : this.archive.read('target', callId);
    if (encoded === null) return null;
    const value = JSON.parse(encoded) as PhoneCallTarget; validatePhone('PhoneCallTarget', value);
    requireThat(value.callId === callId && this.getCall(callId), 'phone_storage_invalid', 'Phone target belongs to another original admission.');
    return value;
  }
  retainTarget(value: PhoneCallTarget): PhoneCallTarget {
    validatePhone('PhoneCallTarget', value);
    const encoded = canonical(value, phoneFrameBytes - 512);
    return this.transaction(() => {
      const prior = this.target(value.callId);
      if (prior) {
        requireThat(canonical(prior) === encoded, 'mutation_conflict', 'Original Phone target cannot be replaced.'); return prior;
      }
      requireThat(this.currentCall(value.callId), 'phone_call_conflict', 'Only the original current admission can retain its target.');
      requireThat(this.status().reservedBytes + phoneOutcomeReservation <= this.limits.maxBytes,
        'phone_journal_capacity', 'Phone cannot reserve its original target before Voice effects.');
      this.statement('INSERT INTO targets VALUES (?,?)').run(value.callId, encoded);
      this.setMeta('targets', String(Number(this.meta('targets')) + 1));
      return JSON.parse(encoded) as PhoneCallTarget;
    });
  }
  getCall(callId: string): PhoneCall | null {
    validate('Uuid', callId);
    const row = this.statement('SELECT value FROM calls WHERE call_id=?').get(callId);
    const encoded = row ? String(row['value']) : this.archive.read('call', callId);
    if (encoded === null) return null;
    const value = JSON.parse(encoded) as PhoneCall; validatePhoneAdmission('PhoneCall', value);
    requireThat(value.callId === callId, 'phone_storage_invalid', 'Phone admission belongs to another original call.'); return value;
  }
  history(principalId: string | null, limit: number): PhoneCall[] {
    requireThat(Number.isInteger(limit) && limit >= 1 && limit <= 64, 'invalid_arguments', 'History limit must be between 1 and 64.');
    const rows = this.statement(`SELECT value FROM (
      SELECT value,0 AS section,rowid AS sequence FROM calls
      UNION ALL SELECT value,1 AS section,rowid AS sequence FROM receipt_archive WHERE kind='call'
    ) WHERE (? IS NULL OR json_extract(value,'$.principalId')=?) ORDER BY section,sequence DESC LIMIT ?`).all(principalId, principalId, limit);
    const calls: PhoneCall[] = []; let bytes = 0;
    for (const row of rows) {
      const value = String(row['value']); bytes += Buffer.byteLength(value) + 1;
      if (bytes > 128 * 1024) break;
      const call = JSON.parse(value) as PhoneCall; validatePhoneAdmission('PhoneCall', call); calls.push(call);
    }
    return calls;
  }
  callForOperation(operationId: string): PhoneCall | null {
    validate('Uuid', operationId);
    const row = this.statement('SELECT call_id FROM calls WHERE operation_id=?').get(operationId);
    const id = row ? String(row['call_id']) : this.archive.read('call-operation', operationId);
    const call = id ? this.getCall(id) : null;
    requireThat(!call || call.operationId === operationId, 'phone_storage_invalid', 'Phone admission has another original operation.'); return call;
  }
  currentCall(callId?: string): PhoneCall | null {
    const row = callId ? this.statement('SELECT call_id FROM active_calls WHERE call_id=?').get(callId) : this.statement('SELECT call_id FROM active_calls LIMIT 1').get();
    const call = row ? this.getCall(String(row['call_id'])) : null;
    return call?.epoch === this.epoch ? call : null;
  }
  currentCalls(): PhoneCall[] { return this.statement('SELECT call_id FROM active_calls').all().map(row => this.currentCall(String(row['call_id']))).filter((call): call is PhoneCall => call !== null); }
  callCommand(callId: string, method: PhoneJournalIntent['method'], generation = 0): PhoneOperation | null {
    validate('Uuid', callId);
    requireThat(Number.isInteger(generation) && generation >= 0 && generation <= 128, 'invalid_arguments', 'Bounded Voice generation required.');
    const row = this.statement("SELECT operation_id FROM commands WHERE call_id=? AND method=? AND COALESCE(json_extract(value,'$.intent.voiceGeneration'),0)=? ORDER BY rowid LIMIT 1").get(callId, method, generation);
    const id = row ? String(row['operation_id']) : this.archive.read('call-command', canonical(generation === 0 ? [callId, method] : [callId, method, generation]));
    return id ? this.get(id) : null;
  }
  voiceSelectionCommand(callId: string, commandSequence: number): PhoneOperation | null {
    validate('Uuid', callId);
    requireThat(Number.isSafeInteger(commandSequence) && commandSequence >= 1 && commandSequence <= 2147483647,
      'invalid_arguments', 'Positive bounded Voice selection command sequence required.');
    const row = this.statement("SELECT operation_id FROM commands WHERE call_id=? AND method='call.selectVoice' AND json_extract(value,'$.intent.commandSequence')=? LIMIT 1")
      .get(callId, commandSequence);
    const id = row ? String(row['operation_id']) : this.archive.read('voice-selection', canonical([callId, commandSequence]));
    return id ? this.get(id) : null;
  }
  feedbackCommand(callId: string, commandSequence: number): PhoneOperation | null {
    validate('Uuid', callId);
    requireThat(Number.isSafeInteger(commandSequence) && commandSequence >= 1 && commandSequence <= 2147483647,
      'invalid_arguments', 'Positive bounded command feedback sequence required.');
    const row = this.statement("SELECT operation_id FROM commands WHERE call_id=? AND method='call.command.feedback' AND json_extract(value,'$.intent.commandSequence')=? LIMIT 1")
      .get(callId, commandSequence);
    const id = row ? String(row['operation_id']) : this.archive.read('command-feedback', canonical([callId, commandSequence]));
    return id ? this.get(id) : null;
  }
  voiceTask(callId: string, generation: number): string | null {
    const operation = this.callCommand(callId, 'call.bindVoice', generation);
    const result = operation?.phase === 'result' && operation.receipt?.ok ? operation.receipt.result as { threadId?: unknown; bound?: unknown } : null;
    requireThat(!operation || result?.bound === true && typeof result.threadId === 'string' && operation.intent.method === 'call.bindVoice' &&
      operation.intent.threadId === result.threadId, 'phone_storage_invalid', 'Voice task binding receipt is invalid.');
    return (result?.threadId as string | undefined) ?? null;
  }
  createdVoiceTask(callId: string, generation: number): string | null {
    const operation = this.callCommand(callId, 'call.createVoice', generation);
    if (!operation) return null;
    requireThat(operation.intent.method === 'call.createVoice', 'phone_storage_invalid', 'Voice creation method changed.');
    if (operation.phase !== 'result') throw new IvyError('phone_voice_create_unknown', 'Original task creation is unresolved; no replacement task may be created.', 'unknown');
    const result = operation.receipt?.result as { threadId?: unknown; created?: unknown } | null;
    requireThat(result?.created === true && typeof result.threadId === 'string',
      'phone_storage_invalid', 'Voice task creation receipt is invalid.');
    return result.threadId;
  }
  latestVoiceGeneration(callId: string): number | null {
    for (let generation = 128; generation >= 0; generation--) if (this.voiceTask(callId, generation)) return generation;
    return null;
  }
  /** Last positively bound PhoneBridge chat for this caller, including chats
   * started by Desktop. Original receipts also survive process restarts. */
  reusableVoiceTask(principalId: string): string | null {
    validateShared('Identifier', principalId);
    // History puts unresolved calls before archived calls. Order binding receipts
    // themselves so an older unresolved call cannot mask the latest conversation.
    const row = this.statement(`SELECT json_extract(b.value,'$.intent.callId') AS call_id,
      COALESCE(json_extract(b.value,'$.intent.voiceGeneration'),0) AS generation FROM (
        SELECT value FROM commands WHERE method='call.bindVoice'
        UNION ALL SELECT value FROM receipt_archive WHERE kind='command'
          AND json_extract(value,'$.intent.method')='call.bindVoice'
      ) b JOIN (
        SELECT call_id,value FROM calls
        UNION ALL SELECT key AS call_id,value FROM receipt_archive WHERE kind='call'
      ) c ON c.call_id=json_extract(b.value,'$.intent.callId')
      WHERE json_extract(c.value,'$.principalId')=? AND json_extract(b.value,'$.phase')='result'
        AND json_extract(b.value,'$.receipt.ok')=1
      ORDER BY json_extract(b.value,'$.updatedAt') DESC LIMIT 1`).get(principalId);
    return row ? this.voiceTask(String(row['call_id']), Number(row['generation'])) : null;
  }
  retiredVoiceTask(threadId: string): RetiredVoiceTask | null {
    validate('Uuid', threadId);
    const value = this.meta(`voiceRetired:${threadId}`);
    return value ? this.retiredRow(JSON.parse(value) as RetiredVoiceTask) : null;
  }
  retiredVoicePage(limit = 16, after = ''): RetiredVoiceTask[] {
    requireThat(Number.isInteger(limit) && limit >= 1 && limit <= 64,
      'invalid_arguments', 'Retired Voice scan requires a bounded page.');
    return this.statement(`SELECT value FROM meta WHERE key>? AND key GLOB 'voiceRetired:*' AND
      json_extract(value,'$.phase') IN ('pending_archive','archiving')
      ORDER BY key LIMIT ?`).all(`voiceRetired:${after}`, limit)
      .map(row => this.retiredRow(JSON.parse(String(row['value'])) as RetiredVoiceTask));
  }
  private retiredRow(task: RetiredVoiceTask): RetiredVoiceTask {
    validate('Uuid', task.threadId); validate('Uuid', task.successorId); validate('Uuid', task.callId);
    requireThat(['pending_archive','archiving','archived','deleting','deleted','retained'].includes(task.phase) &&
      !!this.getCall(task.callId) && task.threadId !== task.successorId &&
      (task.archiveOperationId === null || typeof task.archiveOperationId === 'string') &&
      (task.deleteOperationId === null || typeof task.deleteOperationId === 'string') &&
      (task.archivedAt === null || typeof task.archivedAt === 'string') &&
      (task.deleteAfter === null || typeof task.deleteAfter === 'string'),
      'phone_storage_invalid', 'Retired Voice task evidence is invalid.');
    return task;
  }
  beginVoiceArchive(threadId: string, operationId: string): void {
    validate('Uuid', operationId);
    this.transaction(() => {
      const task = this.retiredVoiceTask(threadId);
      requireThat(task?.phase === 'pending_archive', 'phone_voice_retention_changed', 'Voice task is not awaiting archival.');
      this.setMeta(`voiceRetired:${threadId}`, canonical({ ...task, phase: 'archiving', archiveOperationId: operationId }));
    });
  }
  finishVoiceArchive(threadId: string, at: string): void {
    requireThat(Number.isFinite(Date.parse(at)), 'invalid_arguments', 'Archive time is invalid.');
    this.transaction(() => {
      const task = this.retiredVoiceTask(threadId);
      requireThat(task && ['pending_archive','archiving','archived'].includes(task.phase),
        'phone_voice_retention_changed', 'Voice task archive state changed.');
      if (task.phase === 'archived') return;
      this.setMeta(`voiceRetired:${threadId}`, canonical({ ...task, phase: 'archived', archivedAt: at, deleteAfter: null }));
    });
  }
  /** No remote cleanup is inferred: only a never-created outgoing call can free this slot. */
  discardUnpreparedCall(callId: string): void {
    validate('Uuid', callId);
    this.transaction(() => {
      const call = this.getCall(callId);
      requireThat(call, 'phone_release_unconfirmed', 'Original call admission is required.');
      const count = Number(this.statement('SELECT count(*) AS count FROM commands WHERE call_id=?').get(callId)!['count']);
      if (call.direction === 'incoming') {
        const claim = this.callCommand(callId, 'call.claim');
        requireThat(count === 0 || count === 1 && claim?.phase === 'result' && claim.receipt?.ok === false && claim.receipt.error === 'phone_offer_expired',
          'phone_release_unconfirmed', 'A claimed or uncertain incoming offer still requires native cleanup.');
        this.statement('DELETE FROM active_calls WHERE call_id=?').run(callId); return;
      }
      const prepare = this.callCommand(callId, 'call.prepare');
      requireThat(count === 0 || count === 1 && prepare?.phase === 'result' && prepare.receipt?.ok === false &&
        ['operation_conflict', 'runtime_not_ready'].includes(prepare.receipt.error!),
      'phone_release_unconfirmed', 'Possible native call creation requires original cleanup.');
      this.statement('DELETE FROM active_calls WHERE call_id=?').run(callId);
    });
  }
  /** Policy is checked by the service before this atomic admission and before any native effect. */
  admitCall(admission: PhoneCallAdmission): PhoneCall {
    validatePhoneAdmission('PhoneCallAdmission', admission);
    requireThat(admission.direction === 'incoming' || admission.callId === null, 'invalid_arguments', 'Outgoing call UUIDs are allocated by original admission.');
    this.compact();
    return this.transaction(() => {
      const prior = this.callForOperation(admission.operationId);
      if (prior) {
        const original = { ...prior, callId: prior.direction === 'outgoing' ? null : prior.callId };
        requireThat(canonical(original) === canonical(admission), 'mutation_conflict', 'Phone admission arguments or ownership changed.'); return prior;
      }
      requireThat(admission.epoch === this.epoch, 'phone_epoch_mismatch', 'Call admission requires the current native epoch.');
      const active = this.currentCalls();
      requireThat(active.length < this.maxConcurrentCalls, 'phone_call_busy', 'Configured call concurrency is occupied.');
      requireThat(!admission.callId || !this.getCall(admission.callId), 'phone_call_conflict', 'An incoming call already has its original admission.');
      const usage = this.status();
      requireThat(usage.calls < this.limits.maxOperations && usage.operations + active.length * 4 + 16 <= this.limits.maxOperations &&
        usage.reservedBytes + phoneCallReservation + (17 + active.length * 4) * phoneOutcomeReservation <= this.limits.maxBytes,
        'phone_journal_capacity', 'Phone call admission history is full.');
      const call: PhoneCall = { ...structuredClone(admission), callId: admission.callId ?? randomUUID() };
      validatePhoneAdmission('PhoneCall', call);
      this.statement('INSERT INTO calls VALUES (?,?,?)').run(call.callId, call.operationId, canonical(call, phoneCallReservation));
      this.setMeta('calls', String(usage.calls + 1)); this.statement('INSERT INTO active_calls VALUES (?)').run(call.callId); return call;
    });
  }
  /** Only the saved exact native release receipt can free the same-epoch slot. */
  releaseCall(callId: string, releaseOperationId: string): void {
    validate('Uuid', callId); validate('Uuid', releaseOperationId);
    this.transaction(() => {
      const call = this.getCall(callId), release = this.get(releaseOperationId), result = release?.receipt?.result;
      requireThat(call && release?.phase === 'result' && release.intent.epoch === call.epoch && release.intent.callId === callId &&
        release.intent.method === 'call.release' && release.receipt?.ok === true && release.receipt.error === null && result !== null && typeof result === 'object' &&
        'callId' in result && result.callId === callId && 'released' in result && result.released === true,
      'phone_release_unconfirmed', 'Call slot requires its original confirmed native release.');
      this.statement('DELETE FROM active_calls WHERE call_id=?').run(callId);
    });
  }
  /** OS ownership/fences must be resolved by the process owner before retiring an older epoch. */
  beginEpoch(epoch: string): void {
    validate('Uuid', epoch);
    this.compact();
    this.transaction(() => {
      requireThat(this.epoch === null, 'phone_epoch_active', 'The original Phone epoch must be retired before another process starts.');
      requireThat(!this.statement('SELECT epoch FROM epochs WHERE epoch=?').get(epoch) && !this.archive.read('epoch', epoch), 'mutation_conflict', 'A Phone process epoch can never be reused.');
      const count = this.status().epochs;
      requireThat(count < this.limits.maxEpochs, 'phone_journal_capacity', 'Phone epoch history is full.');
      this.statement('INSERT INTO epochs VALUES (?)').run(epoch); this.setMeta('epochs', String(count + 1)); this.setMeta('epoch', epoch); this.setMeta('epochOperations', '0');
    });
  }
  /** A delayed old close cannot retire a new process. Lost intents remain permanently unknown. */
  loseEpoch(epoch: string): void {
    validate('Uuid', epoch);
    this.transaction(() => {
      if (epoch !== this.epoch) return;
      const now = new Date().toISOString();
      while (true) {
        const row = this.statement("SELECT operation_id FROM commands WHERE epoch=? AND phase='submitted' LIMIT 1").get(epoch);
        if (!row) break;
        const value = this.get(String(row['operation_id']))!;
        this.save({ ...value, phase: 'outcome_unknown', updatedAt: now });
      }
      this.setMeta('epoch', '');
      this.db.exec('DELETE FROM active_calls;');
    });
  }
  get(operationId: string): PhoneOperation | null {
    validate('Uuid', operationId);
    const row = this.statement('SELECT value FROM commands WHERE operation_id=?').get(operationId);
    const encoded = row ? String(row['value']) : this.archive.read('command', operationId);
    if (encoded === null) return null;
    const value = JSON.parse(encoded) as PhoneOperation; validate('Operation', value); intentShape(value.intent);
    requireThat(value.intent.operationId === operationId, 'phone_storage_invalid', 'Phone evidence has a different original operation identity.');
    return value;
  }
  private save(value: PhoneOperation): PhoneOperation {
    validate('Operation', value); const encoded = canonical(value, phoneOutcomeReservation);
    this.statement('UPDATE commands SET phase=?,value=? WHERE operation_id=?').run(value.phase, encoded, value.intent.operationId); return value;
  }
  /** This hook admits one possible write, not a transport retry. Read saved originals with get(). */
  submit(intent: PhoneJournalIntent): void {
    intentShape(intent);
    this.compact();
    this.transaction(() => {
      const prior = this.get(intent.operationId);
      if (prior) {
        requireThat(canonical(prior.intent) === canonical(intent), 'mutation_conflict', 'The retained Phone operation has different original arguments or ownership.');
        const result = prior.receipt?.result;
        const unknown = prior.phase !== 'result' || prior.receipt?.error === 'operation_outcome_unknown' ||
          (result !== null && typeof result === 'object' && ('state' in result && result.state === 'outcome_unknown' ||
            'result' in result && result.result !== null && typeof result.result === 'object' &&
              ('phase' in result.result && result.result.phase === 'outcome_unknown' || 'state' in result.result && result.result.state === 'outcome_unknown')));
        throw new IvyError('phone_operation_retained', 'Read the retained original Phone outcome; this command cannot be sent again.', unknown ? 'unknown' : 'completed');
      }
      requireThat(intent.epoch === this.epoch, 'phone_epoch_mismatch', 'Only the active Phone process can admit a command.');
      if (intent.callId !== null) {
        const admitted = this.getCall(intent.callId);
        if (admitted && !['call.hangup', 'call.release', 'call.stopVoice', 'call.realtime.stop', 'call.desktop.stopVoice'].includes(intent.method)) {
          requireThat(this.currentCall(admitted.callId) && admitted.epoch === intent.epoch,
            'phone_call_conflict', 'Only the original active admission can issue another positive call action.');
          requireThat(admitted.direction === 'outgoing' ? intent.method !== 'call.answer' : !['call.prepare', 'call.dial'].includes(intent.method),
            'phone_call_conflict', 'Phone command direction differs from the original call admission.');
        }
        const previous = this.statement('SELECT epoch FROM commands WHERE call_id=? LIMIT 1').get(intent.callId);
        requireThat((!admitted || admitted.epoch === intent.epoch) && (!previous || previous['epoch'] === intent.epoch), 'phone_call_conflict', 'A Phone call identity cannot move to another process epoch.');
        if (intent.method.startsWith('call.realtime.') || intent.method.startsWith('call.screening.') || ['call.stopVoice', 'call.features', 'call.windows.connect', 'call.codec.upgrade', 'call.waiting.end', 'call.archiveVoice', 'call.createVoice', 'call.bindVoice', 'call.promptVoice'].includes(intent.method) || intent.method.startsWith('call.desktop.') || intent.method === 'call.prepare' || intent.method === 'call.claim' || intent.method === 'call.audio.prepare' || intent.method === 'call.audio.rebind' || intent.method === 'call.dial' || intent.method === 'call.answer') {
          const methods = intent.method === 'call.dial' || intent.method === 'call.answer' ? ['call.dial', 'call.answer'] : [intent.method, intent.method];
          requireThat(!this.statement("SELECT operation_id FROM commands WHERE call_id=? AND method IN (?,?) AND COALESCE(json_extract(value,'$.intent.voiceGeneration'),0)=? LIMIT 1").get(intent.callId, ...methods, intent.voiceGeneration ?? 0),
            'phone_call_conflict', 'The original call already has its prepare or connect attempt; a new operation cannot repeat it.');
        }
        if (intent.method === 'call.selectVoice' && intent.commandSequence !== undefined)
          requireThat(!this.voiceSelectionCommand(intent.callId, intent.commandSequence),
            'phone_call_conflict', 'This Voice selection command was already submitted.');
        if (intent.method === 'call.restartVoice')
          requireThat(!this.callCommand(intent.callId, intent.method, intent.voiceGeneration),
            'phone_call_conflict', 'This Voice generation already has its restart command.');
        if (intent.method === 'call.command.feedback')
          requireThat(!this.feedbackCommand(intent.callId, intent.commandSequence!),
            'phone_call_conflict', 'This original keypad command already has feedback.');
      }
      const usage = this.status();
      const cleanup = ['call.hangup', 'call.release', 'call.stopVoice', 'call.realtime.stop', 'call.desktop.stopVoice', 'call.archiveVoice'].includes(intent.method);
      const reserve = Math.max(0, this.currentCalls().length * 4 - (cleanup ? 4 : 0));
      requireThat(usage.operations + reserve < this.limits.maxOperations && usage.reservedBytes + (reserve + 1) * phoneOutcomeReservation <= this.limits.maxBytes,
        'phone_journal_capacity', 'Phone cannot reserve another complete command outcome.');
      const now = new Date().toISOString(), value: PhoneOperation = { schemaVersion: 1, intent: structuredClone(intent), phase: 'submitted', receipt: null, createdAt: now, updatedAt: now };
      validate('Operation', value);
      this.statement('INSERT INTO commands VALUES (?,?,?,?,?,?)').run(intent.operationId, intent.epoch, intent.callId, intent.method, value.phase, canonical(value, phoneOutcomeReservation));
      this.setMeta('count', String(usage.operations + 1));
      this.setMeta('epochOperations', String(this.epochOperations + 1));
    });
  }
  finish(intent: PhoneIntent, reply: PhoneReply): PhoneOperation {
    intentShape(intent); validatePhone('Response', reply);
    requireThat(reply.epoch === intent.epoch && reply.ok === (reply.error === null) && (reply.ok || reply.result === null),
      'phone_receipt_conflict', 'Phone reply differs from the original owner or result envelope.');
    return this.finishReceipt(intent, { ok: reply.ok, result: reply.result, error: reply.error });
  }
  /** App Tools effects share the original journal but are never sent to native IPC. */
  finishArchive(intent: PhoneJournalIntent & { method: 'call.archiveVoice' }, result: PhoneVoiceArchiveResult): PhoneOperation {
    validate('PhoneVoiceArchiveResult', result);
    const planned = intent.archivePlan.candidates.map(candidate => candidate.threadId);
    requireThat(canonical(result.archivedIds) === canonical(planned.slice(0, result.archivedIds.length)) &&
      (result.state !== 'archived' || result.archivedIds.length === planned.length && result.failedThreadId === null),
    'phone_receipt_conflict', 'Archive acknowledgements must match the original ordered plan; partial work is never complete.');
    return this.finishReceipt(intent, { ok: true, result, error: null });
  }
  finishVoiceBinding(intent: PhoneJournalIntent & { method: 'call.bindVoice' }, startupThreadId?: string): PhoneOperation {
    if (startupThreadId !== undefined) {
      validate('Uuid', startupThreadId);
      requireThat(startupThreadId !== intent.threadId, 'phone_voice_task_changed',
        'The transient startup chat must differ from the reused chat.');
    }
    const result = { threadId: intent.threadId, bound: true as const }; validate('PhoneVoiceBindingResult', result);
    return this.finishReceipt(intent, { ok: true, result, error: null }, startupThreadId);
  }
  finishVoiceCreation(intent: PhoneJournalIntent & { method: 'call.createVoice' }, threadId: string): PhoneOperation {
    const result = { threadId, created: true as const }; validate('PhoneVoiceCreationResult', result);
    return this.finishReceipt(intent, { ok: true, result, error: null });
  }
  finishVoiceStop(intent: PhoneJournalIntent & { method: 'call.stopVoice' }): PhoneOperation {
    return this.finishReceipt(intent, { ok: true, result: { threadId: intent.threadId, stopped: true }, error: null });
  }
  finishVoicePrompt(intent: PhoneJournalIntent & { method: 'call.promptVoice' | 'call.forwardVoice' }, state: 'sent' | 'outcome_unknown'): PhoneOperation {
    const result = { threadId: intent.threadId, state }; validate('PhoneVoicePromptResult', result);
    return this.finishReceipt(intent, { ok: true, result, error: null });
  }
  finishVoiceSelection(intent: PhoneJournalIntent & { method: 'call.selectVoice' }, state: 'sent' | 'outcome_unknown'): PhoneOperation {
    const result = { threadId: intent.threadId, state }; validate('PhoneVoicePromptResult', result);
    return this.finishReceipt(intent, { ok: true, result, error: null });
  }
  finishVoiceRestart(intent: PhoneJournalIntent & { method: 'call.restartVoice' }, threadId: string | null): PhoneOperation {
    if (threadId !== null) validate('Uuid', threadId);
    return this.finishReceipt(intent, threadId === null
      ? { ok: false, result: null, error: 'operation_outcome_unknown' }
      : { ok: true, result: { threadId, restarted: true }, error: null });
  }
  unresolvedArchive(): boolean {
    // A restart may retire native ownership; it cannot prove a Desktop mutation failed.
    return !!this.statement(`SELECT 1 FROM commands WHERE method='call.archiveVoice' AND
      (phase!='result' OR json_extract(value,'$.receipt.result.state') IS NOT 'archived') AND
      json_extract(value,'$.archiveResolution.state') IS NOT 'observed_archived' LIMIT 1`).get();
  }
  /** Only the archive domain's read-only checks call this. Original receipts/uncertainty
   * remain immutable; this records the desired state observed later, not a fabricated ACK. */
  resolveArchive(operationId: string): PhoneOperation {
    return this.transaction(() => {
      const original = this.get(operationId);
      requireThat(original?.intent.method === 'call.archiveVoice', 'phone_archive_missing', 'Original archive intent required.');
      if (original.archiveResolution) return original;
      const now = new Date().toISOString();
      return this.save({ ...original, archiveResolution: { state: 'observed_archived', checkedAt: now }, updatedAt: now });
    });
  }
  private finishReceipt(intent: PhoneJournalIntent, receipt: NonNullable<PhoneOperation['receipt']>, startupThreadId?: string): PhoneOperation {
    intentShape(intent); validate('Receipt', receipt);
    return this.transaction(() => {
      const prior = this.get(intent.operationId);
      requireThat(prior && canonical(prior.intent) === canonical(intent), 'phone_receipt_conflict', 'Phone reply has no matching original intent.');
      if (prior.phase === 'result') {
        requireThat(canonical(prior.receipt) === canonical(receipt), 'phone_receipt_conflict', 'Phone cannot replace a saved original result.'); return prior;
      }
      requireThat(prior.phase === 'submitted' && this.epoch === intent.epoch, 'phone_receipt_conflict', 'A retired Phone owner cannot resolve an unknown operation.');
      const saved = this.save({ ...prior, phase: 'result', receipt: structuredClone(receipt), updatedAt: new Date().toISOString() });
      if (intent.method === 'call.bindVoice' && receipt.ok && intent.callId) {
        const call = this.getCall(intent.callId);
        requireThat(call, 'phone_storage_invalid', 'Bound Voice task lost its original call.');
        const previous = this.meta(`voiceReuse:${call.principalId}`);
        if (previous) {
          const old = JSON.parse(previous) as { threadId?: unknown; callId?: unknown; generation?: unknown };
          requireThat(typeof old.threadId === 'string' && typeof old.callId === 'string' && Number.isInteger(old.generation),
            'phone_storage_invalid', 'Previous Voice task pointer is invalid.');
          validate('Uuid', old.threadId);
          if (old.threadId !== intent.threadId &&
            this.createdVoiceTask(old.callId, Number(old.generation)) === old.threadId)
            this.retireVoiceTask(call, old.threadId, intent.threadId);
        }
        this.setMeta(`voiceReuse:${call.principalId}`, canonical({
          threadId: intent.threadId, callId: intent.callId,
          generation: intent.voiceGeneration ?? 0,
        }));
        if (startupThreadId) this.retireVoiceTask(call, startupThreadId, intent.threadId);
      }
      return saved;
    });
  }
  private retireVoiceTask(call: PhoneCall, threadId: string, successorId: string): void {
    if (!this.meta(`voiceRetired:${threadId}`)) this.setMeta(`voiceRetired:${threadId}`, canonical({
      threadId, principalId: call.principalId, successorId, callId: call.callId,
      phase: 'pending_archive', archiveOperationId: null, deleteOperationId: null,
      archivedAt: null, deleteAfter: null,
    } satisfies RetiredVoiceTask));
  }
}
