import type { Pin, Message, SourceDefinition } from './schema.js';
import type { MicrosoftBinding, MicrosoftProfile, OutlookCall, OutlookPublication } from './microsoft-schema.js';
import type { NativeTarget } from '../../../packages/sdk/src/node.js';
export interface TeamsChat { id: string; title: string }
export interface TeamsSelection { since: string; until: string; chat: TeamsChat | null }
export interface TeamsCollector { sourceId: string; target: NativeTarget; expectedAccountHash: string; profile: MicrosoftProfile; since: string;
  maximumChats: number; maximumMessages: number; excludedChatIds: string[]; pollMs: number; threadCwd: string }
export interface TeamsConfiguration { schemaVersion: 1; collectors: TeamsCollector[] }
export interface TeamsPage { schemaVersion: 1; sourceId: string; accountId: string; observedAt: string; selection: TeamsSelection; chats: TeamsChat[]; messages: Message[];
  excluded: { hiddenChat: number; configuredChat: number; ownSender: number; senderNotAllowed: number; deleted: number; system: number; outsideWindow: number }; messagesWithAttachments: number }
export interface TeamsContinuation { selection: TeamsSelection; remainingChats: TeamsChat[] }
export interface TeamsHead { schemaVersion: 1; configuration: TeamsCollector; source: SourceDefinition; pending: Pin | null; lastPage: Pin | null; continuation: TeamsContinuation | null; nextPollAt: string | null; lastCompleteAt: string | null }
export interface TeamsPlan { schemaVersion: 1; head: Pin; configuration: TeamsCollector; source: SourceDefinition; sourceCheckpoint: Pin; previousCursor: string | null;
  selection: TeamsSelection; epoch: string; preparedAt: string; bindings: MicrosoftBinding[] }
export type TeamsCall = OutlookCall;
export interface TeamsEvidence { schemaVersion: 1; plan: Pin; calls: Pin[]; observedAt: string; gap: string | null; retryAfterSeconds: number | null; page: TeamsPage | null }
export type TeamsPublication = OutlookPublication;
export interface TeamsValues { 'secretary/teams-head': TeamsHead; 'secretary/teams-plan': TeamsPlan; 'secretary/teams-call': TeamsCall; 'secretary/teams-evidence': TeamsEvidence; 'secretary/teams-publication': TeamsPublication }
export const teamsDefinitions = { 'secretary/teams-head': 'TeamsHead', 'secretary/teams-plan': 'TeamsPlan', 'secretary/teams-call': 'TeamsCall', 'secretary/teams-evidence': 'TeamsEvidence', 'secretary/teams-publication': 'TeamsPublication' } as const;
