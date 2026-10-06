import { IvyError, serviceTools } from '../../../packages/sdk/src/node.js';
import type { Wire } from '../../../packages/sdk/src/node.js';
import type { SecretaryEngine } from './engine.js';
import type { Assignment, Pin } from './schema.js';

export const messageWindowKey = 'secretary/channel-window';
export const messageBatchKind = 'secretary.channel.batch';
export const messageWindowMs = 5 * 60_000;

export interface MessageChannel {
  key: string;
  reference: Wire.Json;
  occurredAt: string;
  itemId: string | null;
}
export interface MessageWindow {
  assignment: Pin;
  assignmentSnapshot: Assignment;
  firstSequence: number;
  firstMessageAt: string;
  dueAt: string;
  serviceNodeId: string;
  channel: MessageChannel;
  itemIds: string[];
  eventIds: string[];
}
export interface MessageWindowState { assignmentId: string; lastSequence: number; pending: MessageWindow | null }
export interface MessageBatch { kind: typeof messageBatchKind; dueAt: string; serviceNodeId: string;
  channel: MessageChannel; itemIds: string[]; eventIds: string[] }

const object = (value: unknown): Record<string, Wire.Json> => value && typeof value === 'object' && !Array.isArray(value) ? value as Record<string, Wire.Json> : {};

// Producers advertise a channel in their registered event payload. Secretary never
// needs to know which application produced it or how that application checks unread state.
export function messageChannel(payload: unknown): MessageChannel | null {
  const value = object(object(payload)['messageChannel']);
  return typeof value['key'] === 'string' && value['key'].length > 0 &&
    typeof value['occurredAt'] === 'string' && Number.isFinite(Date.parse(value['occurredAt'])) &&
    value['reference'] !== null && typeof value['reference'] === 'object' && !Array.isArray(value['reference']) &&
    (value['itemId'] === undefined || value['itemId'] === null || typeof value['itemId'] === 'string')
    ? { key: value['key'], reference: value['reference'], occurredAt: value['occurredAt'], itemId: value['itemId'] as string | null ?? null }
    : null;
}

export function messageBatch(payload: unknown): MessageBatch | null {
  const value = object(payload), channel = messageChannel({ messageChannel: value['channel'] });
  return value['kind'] === messageBatchKind && typeof value['dueAt'] === 'string' &&
    typeof value['serviceNodeId'] === 'string' && channel &&
    Array.isArray(value['itemIds']) && value['itemIds'].every(id => typeof id === 'string') &&
    Array.isArray(value['eventIds']) && value['eventIds'].every(id => typeof id === 'string')
    ? value as unknown as MessageBatch : null;
}

export function messageUnreadSnapshot(observation: Wire.Json, batch: MessageBatch): boolean {
  const value = object(observation), observedAt = value['observedAt'];
  if (value['complete'] !== true || typeof value['unread'] !== 'boolean' ||
    typeof observedAt !== 'string' || !Number.isFinite(Date.parse(observedAt)) ||
    Date.parse(observedAt) < Date.parse(batch.dueAt))
    throw new IvyError('secretary_message_unread_pending', 'The channel unread state is incomplete or stale.');
  return value['unread'];
}

export async function messageConversationUnread(engine: SecretaryEngine, batch: MessageBatch): Promise<boolean> {
  try {
    const node = await engine.client.request('serviceNodes.get', { serviceNodeId: batch.serviceNodeId });
    if (!node.connected || !node.synced || !node.ready || node.stale)
      throw new IvyError('secretary_message_unread_pending', 'The message source is unavailable.');
    const source = serviceTools(engine.client, batch.serviceNodeId, [{ namespace: 'message-channel', interfaceVersion: '1.0.0' }]);
    const observation = await source.call('message-channel.unread', {
      rootObjectId: engine.settings.identity.scope.rootObjectId,
      reference: batch.channel.reference,
      notBefore: batch.dueAt,
    });
    return messageUnreadSnapshot(observation, batch);
  } catch {
    throw new IvyError('secretary_message_unread_pending', 'The channel unread state cannot be verified.');
  }
}
