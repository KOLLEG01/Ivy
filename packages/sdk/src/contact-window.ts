import type { RpcClient, RequestOptions } from './client.js';

export interface ContactWindow { timeZone: string; start: string; end: string }

/** Compare local wall-clock time, including windows crossing midnight and DST. */
export function contactWindowOpen(rule: { window: ContactWindow | null }, now: Date): boolean {
  if (!rule.window) return true;
  const { timeZone, start, end } = rule.window;
  const parts = new Intl.DateTimeFormat('en', { timeZone, hour: '2-digit', minute: '2-digit', hourCycle: 'h23', numberingSystem: 'latn' }).formatToParts(now);
  const at = parts.find(value => value.type === 'hour')!.value + ':' + parts.find(value => value.type === 'minute')!.value;
  return start < end ? at >= start && at < end : at >= start || at < end;
}

/** Live Hive preferences are authoritative for automatic contact by every client. */
export async function automaticNotificationsAllowed(client: RpcClient, now: Date, options?: RequestOptions): Promise<boolean> {
  const { settings } = await client.request('settings.get', {}, options);
  return !settings.quietHours || !contactWindowOpen({ window: settings.quietHours }, now);
}
