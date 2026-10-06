import { canonical, digest, hashJson } from '../../../packages/sdk/src/node.js';
import type { Agent } from '../../../packages/sdk/src/node.js';
import type { SecretaryEngine } from './engine.js';
import type { Pin } from './schema.js';
import { need, same } from './schema.js';
import type { TriageValues } from './triage-schema.js';
import { triageDefinitions, validateTriage } from './triage-schema.js';

export interface TriageDocument<K extends keyof TriageValues> { pin: Pin; value: TriageValues[K] }
export const triageName = (kind: string, value: unknown) => 'triage-' + kind + '-' + hashJson(value).slice(7);
export const triageCheck = (value: unknown, code = 'secretary_triage_evidence_invalid'): void => need(value, code, 'Triage requires its original item, owner, native intent and complete evidence.');

/** Triage is one local current workflow graph; no triage family is registered in Hive. */
export class TriageStore {
  constructor(readonly engine: SecretaryEngine) {}
  async named(key: string, name: string): Promise<Pin | null> { return this.engine.store.technicalNamed(key, name)?.pin ?? null; }
  async read<K extends keyof TriageValues>(key: K, pin: Pin, name?: string, immutable = false): Promise<TriageDocument<K>> {
    const row = this.engine.store.technicalRead<TriageValues[K]>(key, pin); triageCheck(!name || row.name === name); triageCheck(!immutable || pin.revision === 1);
    validateTriage(triageDefinitions[key], row.value); return { pin: row.pin, value: structuredClone(row.value) };
  }
  async find<K extends keyof TriageValues>(key: K, name: string, immutable = false): Promise<TriageDocument<K> | null> {
    const row = this.engine.store.technicalNamed<TriageValues[K]>(key, name); return row ? this.read(key, row.pin, name, immutable) : null;
  }
  async create<K extends keyof TriageValues>(key: K, name: string, value: TriageValues[K], immutable = false): Promise<TriageDocument<K>> {
    validateTriage(triageDefinitions[key], value); const row = this.engine.store.technicalCreate(key, name, value); if (immutable) triageCheck(same(row.value, value));
    return { pin: row.pin, value: row.value as TriageValues[K] };
  }
  async amend<K extends keyof TriageValues>(key: K, current: TriageDocument<K>, value: TriageValues[K]): Promise<TriageDocument<K>> {
    validateTriage(triageDefinitions[key], value); if (same(current.value, value)) return current;
    const row = this.engine.store.technicalAmend(key, current, value); return { pin: row.pin, value: row.value as TriageValues[K] };
  }
  async nativeJson(kind: 'parameters' | 'request' | 'operation', _identity: unknown, pin: Pin): Promise<unknown> {
    const saved = this.engine.store.technicalBlob('secretary/triage-native-' + kind, '', pin); triageCheck(saved && saved.bytes.length > 0 && saved.bytes.length <= 8 * 1024 * 1024);
    try { return JSON.parse(new TextDecoder('utf-8', { fatal: true }).decode(saved!.bytes)); } catch { triageCheck(false); }
  }
  async saveNativeJson(kind: 'parameters' | 'request' | 'operation', identity: unknown, value: unknown): Promise<Pin> {
    const bytes = Buffer.from(canonical(value, 8 * 1024 * 1024)); if (kind === 'operation') validateOperation(value);
    const name = triageName('native-' + kind, [identity, digest(bytes)]); return this.engine.store.technicalSaveBlob('secretary/triage-native-' + kind, name, bytes);
  }
  async binary(name: string, pin?: Pin, maximumBytes = 2 * 1024 * 1024): Promise<{ pin: Pin; bytes: Buffer } | null> {
    triageCheck(Number.isInteger(maximumBytes) && maximumBytes > 0 && maximumBytes <= 8 * 1024 * 1024); const saved = this.engine.store.technicalBlob('secretary/triage-read', name, pin);
    if (!saved) return null; triageCheck(saved.bytes.length > 0 && saved.bytes.length <= maximumBytes); return saved;
  }
  async saveBinary(name: string, bytes: Buffer, maximumBytes = 2 * 1024 * 1024): Promise<Pin> {
    triageCheck(Number.isInteger(maximumBytes) && maximumBytes > 0 && maximumBytes <= 8 * 1024 * 1024 && bytes.length > 0 && bytes.length <= maximumBytes);
    return this.engine.store.technicalSaveBlob('secretary/triage-read', name, bytes);
  }
}

function validateOperation(value: unknown): asserts value is Agent.Operation {
  triageCheck(value && typeof value === 'object' && typeof (value as { operationId?: unknown }).operationId === 'string');
}
