import { need } from './schema.js';
import type { Assessment, Message, Policy, SourceDefinition } from './schema.js';

export function checkPolicy(policy: Policy): void {
  need(Number.isInteger(policy.researchMaxMinutes) && policy.researchMaxMinutes >= 0 && policy.researchMaxMinutes <= 5, 'invalid_arguments', 'Read-only research must be bounded to at most five minutes.');
  if (!policy.silentTime) return;
  need(policy.silentTime.start !== policy.silentTime.end, 'invalid_arguments', 'Silent time must have distinct start and end.');
  try { new Intl.DateTimeFormat('en', { timeZone: policy.silentTime.timeZone }).format(); }
  catch { need(false, 'invalid_arguments', 'Silent time needs a supported IANA timezone.'); }
}
export function quiet(policy: Policy, urgency: Assessment['urgency'], now: Date): boolean {
  if (!policy.silentTime || policy.urgentBypass && urgency === 'critical') return false;
  const { timeZone, start, end } = policy.silentTime;
  const parts = new Intl.DateTimeFormat('en', { timeZone, hour: '2-digit', minute: '2-digit', hourCycle: 'h23', numberingSystem: 'latn' }).formatToParts(now);
  const time = parts.find(value => value.type === 'hour')!.value + ':' + parts.find(value => value.type === 'minute')!.value;
  return start < end ? time >= start && time < end : time >= start || time < end;
}
export function ignored(source: SourceDefinition, message: Message): 'outgoing' | 'own_sender' | 'sender_not_allowed' | null {
  if (message.outgoing) return 'outgoing';
  if (source.ownSenderIds.includes(message.senderId)) return 'own_sender';
  if (source.allowedSenderIds && !source.allowedSenderIds.includes(message.senderId)) return 'sender_not_allowed';
  return null;
}
export function suppression(_source: SourceDefinition, decision: Assessment, policy: Policy): string | null {
  if (decision.notification === 'needs_attention') return 'needs_attention';
  if (decision.notification === 'none') return decision.disposition === 'ignore' || decision.disposition === 'record' ? decision.disposition : 'notification_none';
  if (decision.urgency === 'low') return 'low_urgency';
  const rank = { low: 0, normal: 1, high: 2, critical: 3 } as const;
  if (decision.notification !== 'voice' && rank[decision.urgency] < rank[policy.minimumMainUrgency]) return 'below_configured_urgency';
  return null;
}
