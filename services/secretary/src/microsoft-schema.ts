import { SchemaValidators, bundleLocalSchema } from '../../../packages/sdk/src/node.js';
import type { Wire } from '../../../packages/sdk/src/node.js';
import schema from '../../../specs/schemas/secretary-microsoft.schema.json' with { type: 'json' };
import teamsSchema from '../../../specs/schemas/secretary-teams.schema.json' with { type: 'json' };
import { teamsDefinitions } from './teams-schema.js';
import type { TeamsValues } from './teams-schema.js';
import type { Message, Pin, SourceDefinition } from './schema.js';
import type { NativeTarget } from '../../../packages/sdk/src/node.js';
export interface MicrosoftProfile { id: string; email: string }
export interface OutlookSelection { since: string; until: string; skip: number; pageSize: number }
export interface OutlookPage { schemaVersion: 1; sourceId: string; accountId: string; observedAt: string; selection: OutlookSelection;
  nextIndex: number | null; complete: boolean; messages: Message[]; excluded: { ownSender: number; senderNotAllowed: number }; messagesWithAttachments: number }
export interface OutlookCollector { sourceId: string; target: NativeTarget; expectedAccountHash: string; profile: MicrosoftProfile; since: string; pageSize: number; pollMs: number; maximumPages: number; threadCwd: string }
export interface MicrosoftConfiguration { schemaVersion: 1; collectors: OutlookCollector[] }
export interface OutlookContinuation { selection: OutlookSelection; pageNumber: number }
export interface OutlookHead { schemaVersion: 1; configuration: OutlookCollector; source: SourceDefinition; pending: Pin | null; lastPage: Pin | null; continuation: OutlookContinuation | null; nextPollAt: string | null; lastCompleteAt: string | null }
export const microsoftMethods = ['account/read', 'thread/start', 'mcpServer/tool/call', 'thread/unsubscribe'] as const;
export type MicrosoftMethod = typeof microsoftMethods[number];
export interface MicrosoftBinding { method: MicrosoftMethod; definitionHash: string; outputSchema: Record<string, Wire.Json> }
export interface OutlookPlan { schemaVersion: 1; head: Pin; configuration: OutlookCollector; source: SourceDefinition; sourceCheckpoint: Pin; previousCursor: string | null; selection: OutlookSelection; pageNumber: number; epoch: string; preparedAt: string; bindings: MicrosoftBinding[] }
export interface MicrosoftBlob { byteLength: number; contentHash: string; chunks: Pin[] }
export interface OutlookCall { schemaVersion: 1; plan: Pin; slot: number; method: MicrosoftMethod; params: Record<string, Wire.Json>; operationId: string; definitionHash: string; preparedAt: string; seen: MicrosoftBlob | null; observation: MicrosoftBlob | null }
export interface OutlookEvidence { schemaVersion: 1; plan: Pin; calls: Pin[]; observedAt: string; gap: string | null; retryAfterSeconds: number | null; page: OutlookPage | null }
export interface OutlookPublication { schemaVersion: 1; evidence: Pin; captured: Pin[]; checkpoint: Pin | null }
export interface MicrosoftValues extends TeamsValues { 'secretary/outlook-head': OutlookHead; 'secretary/outlook-plan': OutlookPlan; 'secretary/outlook-call': OutlookCall; 'secretary/outlook-evidence': OutlookEvidence; 'secretary/outlook-publication': OutlookPublication }
export const microsoftDefinitions = { 'secretary/outlook-head': 'OutlookHead', 'secretary/outlook-plan': 'OutlookPlan', 'secretary/outlook-call': 'OutlookCall', 'secretary/outlook-evidence': 'OutlookEvidence', 'secretary/outlook-publication': 'OutlookPublication', ...teamsDefinitions } as const;
const validators = new SchemaValidators();
export const microsoftBundled = (name: string) => bundleLocalSchema(name.startsWith('Teams') ? teamsSchema : schema, name);
export function validateMicrosoft(name: string, value: unknown): void { validators.validate(microsoftBundled(name), value); }
export function microsoftRegistry(): Wire.RegistrySync {
  return { namespaces: [], contracts: [], requiredContracts: [] };
}
