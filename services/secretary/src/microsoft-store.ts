import { canonical, digest, hashJson, nativeInstant, validateAgent, validateNativeProgress } from '../../../packages/sdk/src/node.js';
import type { Agent, NativeOwner } from '../../../packages/sdk/src/node.js';
import { need, same } from './schema.js';
import type { Pin } from './schema.js';
import type { SecretaryStore } from './store.js';
import { microsoftDefinitions, validateMicrosoft } from './microsoft-schema.js';
import type { MicrosoftBlob, MicrosoftValues } from './microsoft-schema.js';

export interface MicrosoftDocument<K extends keyof MicrosoftValues> { pin: Pin; value: MicrosoftValues[K] }
export interface LocalEpochBoundary { schemaVersion: 1; plan: Pin; callHash: string; previousEpoch: string; absence: Agent.OperationAbsence; status: Agent.Status }
const check = (value: unknown) => need(value, 'microsoft_evidence_invalid', 'Microsoft recovery data must retain its original identity and bytes.');
const maximumBytes = 32 * 1024 * 1024;
const blobName = (plan: Pin, slot: number, hash: string) => 'microsoft-operation-' + hashJson([plan, slot, hash]).slice(7);

/** Microsoft collection plans, calls and observations are local Secretary workflow state. */
export class MicrosoftStore {
  constructor(readonly store: SecretaryStore, readonly guard: () => Promise<void>) {}
  async read<K extends keyof MicrosoftValues>(key: K, selected: Pin | string, immutable = false, name?: string): Promise<MicrosoftDocument<K>> {
    const row = this.store.technicalRead<MicrosoftValues[K]>(key, selected); check(!name || row.name === name); check(!immutable || row.pin.revision === 1);
    validateMicrosoft(microsoftDefinitions[key], row.value); return { pin: row.pin, value: structuredClone(row.value) };
  }
  private named(key: string, name: string): Pin | null { return this.store.technicalNamed(key, name)?.pin ?? null; }
  async find<K extends keyof MicrosoftValues>(key: K, name: string, immutable = false): Promise<MicrosoftDocument<K> | null> { const pin = this.named(key, name); return pin ? this.read(key, pin, immutable, name) : null; }
  async create<K extends keyof MicrosoftValues>(key: K, name: string, value: MicrosoftValues[K], immutable = false): Promise<MicrosoftDocument<K>> {
    validateMicrosoft(microsoftDefinitions[key], value); await this.guard(); const row = this.store.technicalCreate(key, name, value); if (immutable) check(same(row.value, value));
    return { pin: row.pin, value: row.value as MicrosoftValues[K] };
  }
  async amend<K extends keyof MicrosoftValues>(key: K, current: MicrosoftDocument<K>, value: MicrosoftValues[K]): Promise<MicrosoftDocument<K>> {
    validateMicrosoft(microsoftDefinitions[key], value); if (same(current.value, value)) return current; await this.guard(); const row = this.store.technicalAmend(key, current, value);
    return { pin: row.pin, value: row.value as MicrosoftValues[K] };
  }
  async checkCallHistory<K extends 'secretary/outlook-call' | 'secretary/teams-call'>(key: K, call: MicrosoftDocument<K>, owner: NativeOwner): Promise<void> {
    const latest = call.value.observation ?? call.value.seen; if (!latest) return;
    const current = await this.operation(call.value.plan, call.value.slot, latest); owner.checkOperation(current, call.value);
    check(nativeInstant(current.createdAt) >= nativeInstant(call.value.preparedAt) && Boolean(call.value.observation) === ['succeeded', 'failed'].includes(current.phase));
    if (call.value.seen && call.value.observation) { const pending = await this.operation(call.value.plan, call.value.slot, call.value.seen); owner.checkOperation(pending, call.value); check(!['succeeded', 'failed'].includes(pending.phase)); validateNativeProgress(pending, current, 'microsoft_evidence_invalid'); }
  }
  async saveOperation(plan: Pin, slot: number, operation: Agent.Operation): Promise<MicrosoftBlob> {
    validateAgent('Operation', operation); const bytes = Buffer.from(canonical(operation, maximumBytes)), contentHash = digest(bytes); await this.guard();
    const pin = this.store.technicalSaveBlob('secretary/microsoft-native', blobName(plan, slot, contentHash), bytes);
    const value = { byteLength: bytes.length, contentHash, chunks: [pin] }; validateMicrosoft('MicrosoftBlob', value); return value;
  }
  async operation(plan: Pin, slot: number, value: MicrosoftBlob): Promise<Agent.Operation> {
    validateMicrosoft('MicrosoftBlob', value); check(value.chunks.length === 1); const saved = this.store.technicalBlob('secretary/microsoft-native', '', value.chunks[0]); check(saved && saved.bytes.length === value.byteLength && digest(saved.bytes) === value.contentHash);
    const text = new TextDecoder('utf-8', { fatal: true }).decode(saved!.bytes), operation = JSON.parse(text) as Agent.Operation; check(canonical(operation, maximumBytes) === text); validateAgent('Operation', operation); return operation;
  }
}

/** A current local absence decision; it is recovery state, never a Hive Object. */
export class MicrosoftEpochBoundaries {
  constructor(readonly store: SecretaryStore, readonly owner: NativeOwner, readonly namespace: string) {}
  private name(plan: Pin, call: { operationId: string; method: string; params: unknown; definitionHash: string }): string {
    return 'epoch-' + hashJson([plan, call.operationId, call.method, call.params, call.definitionHash]).slice(7);
  }
  async read(plan: Pin, call: { operationId: string; method: string; params: unknown; definitionHash: string }, previousEpoch: string): Promise<LocalEpochBoundary | null> {
    const saved = this.store.technicalNamed<LocalEpochBoundary>(this.namespace + '/epoch-boundary', this.name(plan, call));
    if (!saved) return null;
    check(saved.value.schemaVersion === 1 && same(saved.value.plan, plan) && saved.value.previousEpoch === previousEpoch &&
      saved.value.callHash === hashJson({ operationId: call.operationId, method: call.method, params: call.params, definitionHash: call.definitionHash }) &&
      saved.value.absence.operationId === call.operationId && saved.value.status.epoch === saved.value.absence.epoch && saved.value.status.epoch !== previousEpoch);
    return structuredClone(saved.value);
  }
  async record(plan: Pin, call: { operationId: string; method: string; params: unknown; definitionHash: string }, previousEpoch: string,
    absence: Agent.OperationAbsence, guard: () => Promise<void>): Promise<void> {
    await guard(); const status = await this.owner.status();
    const value: LocalEpochBoundary = { schemaVersion: 1, plan, callHash: hashJson({ operationId: call.operationId, method: call.method, params: call.params, definitionHash: call.definitionHash }), previousEpoch, absence, status };
    check(absence.operationId === call.operationId && status.epoch === absence.epoch && status.epoch !== previousEpoch);
    const name = this.name(plan, call), prior = this.store.technicalNamed<LocalEpochBoundary>(this.namespace + '/epoch-boundary', name);
    if (prior) { check(same(prior.value, value)); return; }
    this.store.technicalCreate(this.namespace + '/epoch-boundary', name, value);
  }
}
