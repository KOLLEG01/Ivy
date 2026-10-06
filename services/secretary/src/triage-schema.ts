import { SchemaValidators, bundleLocalSchema } from '../../../packages/sdk/src/node.js';
import type { Wire } from '../../../packages/sdk/src/node.js';
import schema from '../../../specs/schemas/secretary-triage.schema.json' with { type: 'json' };
import type { Assessment, Identity, Message, Pin, Policy, SourceDefinition } from './schema.js';
import type { NativeTarget } from '../../../packages/sdk/src/node.js';
export interface TriageSettings { schemaVersion: 1; target: NativeTarget; model: string; effort: 'low' | 'medium' | 'high' | 'xhigh'; instructions: string; threadCwd: string; maximumConcurrent: number }
export interface TriageMediaAttachment { name: string | null; mediaType: string | null; manifest: Pin | null; byteLength: number | null; contentHash: string | null;
  representation: 'original_bytes' | 'rendered_preview'; kind: 'text' | 'image' | 'audio' | 'video' | 'unavailable'; inputIndex: number | null; errorCode: string | null }
export interface TriageMedia { coverage: Pin; coverageContract: string; coverageHash: string; gap: string | null; attachments: TriageMediaAttachment[] }
export interface TriageInput { item: Pin; source: SourceDefinition; message: Message; contentScope: 'captured_text_or_caption' | 'verified_media'; media: TriageMedia | null; policy: Policy }
export const triageMethods = ['thread/start', 'thread/resume', 'turn/start', 'thread/unsubscribe'] as const;
export type TriageMethod = typeof triageMethods[number];
export interface TriageBinding { method: TriageMethod; definitionHash: string; outputSchema: Record<string, Wire.Json> }
export interface TriageParameters { threadStart: Record<string, Wire.Json>; threadResume: Record<string, Wire.Json>; turnStart: Record<string, Wire.Json> }
export interface TriagePlan { schemaVersion: 1; identity: Identity; settings: TriageSettings; input: TriageInput; prompt: string; promptHash: string; developerInstructions: string; outputSchema: Record<string, Wire.Json>; nativeParams: Pin; bindings: TriageBinding[]; createdAt: string }
export interface TriageWork { schemaVersion: 1; item: Pin; plan: Pin; operationId: string; phase: 'pending' | 'assessed' | 'superseded' | 'failed'; completion: Pin | null; assessment: Pin | null; errorCode: string | null }
export interface TriageCall { schemaVersion: 1; plan: Pin; slot: string; method: TriageMethod; request: Pin; operationId: string; definitionHash: string; preparedAt: string; observation: Pin | null }
export interface TriageCompletion { schemaVersion: 1; plan: Pin; turn: Pin; threadId: string; turnId: string; full: Pin; verification: Pin; result: Assessment; completedAt: string }
export interface TriageThread { schemaVersion: 1; plan: Pin | null; threadId: string | null }
export interface TriageValues { 'secretary/triage-thread': TriageThread; 'secretary/triage-plan': TriagePlan; 'secretary/triage-work': TriageWork; 'secretary/triage-call': TriageCall; 'secretary/triage-completion': TriageCompletion }
export const triageDefinitions = { 'secretary/triage-thread': 'TriageThread', 'secretary/triage-plan': 'TriagePlan', 'secretary/triage-work': 'TriageWork', 'secretary/triage-call': 'TriageCall', 'secretary/triage-completion': 'TriageCompletion' } as const;
const validators = new SchemaValidators();
export const triageBundled = (name: string) => bundleLocalSchema(schema, name);
export function validateTriage(name: string, value: unknown): void { validators.validate(triageBundled(name), value); }
export function triageRegistry(): Wire.RegistrySync {
  return { namespaces: [], contracts: [], requiredContracts: [] };
}
