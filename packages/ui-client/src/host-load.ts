import type { UsageMeter, UsageMeterGroup } from '../../ui/src/lib/usage.js';
import type { Agent, Wire } from '../../sdk/src/client.js';
import { record, text } from './native.js';

export interface UsageWindow { label: string; short: string; used: number; resets: string }
export interface UsageBucket { id: string; name: string; plan: string; windows: UsageWindow[]; notes: string[] }
/** One provider account of a host and its latest limit reading. */
export interface AccountUsage { key: string; label: string; ready: boolean; result: Record<string, Wire.Json> | null; error: string | null }

const windowLabel = (minutes: number) =>
  !Number.isFinite(minutes) || minutes <= 0 ? 'Usage'
    : minutes % 10080 === 0 ? (minutes === 10080 ? 'Weekly limit' : minutes / 10080 + '-week limit')
    : minutes % 1440 === 0 ? (minutes === 1440 ? 'Daily limit' : minutes / 1440 + '-day limit')
    : minutes % 60 === 0 ? minutes / 60 + '-hour limit' : minutes + '-minute limit';
const windowShort = (minutes: number) =>
  !Number.isFinite(minutes) || minutes <= 0 ? '' : minutes % 1440 === 0 ? minutes / 1440 + 'd' : minutes % 60 === 0 ? minutes / 60 + 'h' : minutes + 'm';
const resetLabel = (seconds: unknown) => {
  const value = Number(seconds) * 1000;
  if (!Number.isFinite(value) || value <= 0) return '';
  const date = new Date(value), sameDay = date.toDateString() === new Date().toDateString();
  return 'Resets ' + (sameDay
    ? date.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' })
    : date.toLocaleString([], { weekday: 'short', day: 'numeric', month: 'short', hour: '2-digit', minute: '2-digit' }));
};
const percent = (value: unknown) => Math.min(100, Math.max(0, Math.round(Number(value) || 0)));

/** Native readings report used shares; only an explicit remaining share is inverted. */
export function usageBuckets(value: Record<string, Wire.Json> | null): UsageBucket[] {
  if (!value) return [];
  const byId = record(value.rateLimitsByLimitId);
  const entries = Object.keys(byId).length ? Object.entries(byId) : [['', value.rateLimits] as const];
  return entries.map(([id, raw]) => {
    const limit = record(raw), windows: UsageWindow[] = [], notes: string[] = [];
    for (const part of [limit.primary, limit.secondary]) {
      const window = record(part), minutes = Number(window.windowDurationMins);
      if (typeof window.usedPercent === 'number')
        windows.push({ label: windowLabel(minutes), short: windowShort(minutes), used: percent(window.usedPercent), resets: resetLabel(window.resetsAt) });
    }
    const individual = record(limit.individualLimit);
    if (typeof individual.remainingPercent === 'number')
      windows.push({ label: 'Your limit · ' + text(individual.used) + ' of ' + text(individual.limit), short: 'you',
        used: percent(100 - individual.remainingPercent), resets: resetLabel(individual.resetsAt) });
    const credits = record(limit.credits);
    if (credits.unlimited === true) notes.push('Unlimited credits');
    else if (credits.hasCredits === true && text(credits.balance)) notes.push('Credits: ' + text(credits.balance));
    if (text(limit.rateLimitReachedType)) notes.push('Limit reached: ' + text(limit.rateLimitReachedType).replaceAll('_', ' '));
    if (limit.spendControlReached === true) notes.push('Spend control reached');
    return { id: id || text(limit.limitId), name: text(limit.limitName) || text(limit.limitId) || id || 'Usage', plan: text(limit.planType), windows, notes };
  }).filter(bucket => bucket.windows.length || bucket.notes.length);
}

/** The account's current bucket is shown at a glance; every bucket remains in the details. */
export function coreBucket(value: Record<string, Wire.Json> | null, buckets: UsageBucket[]): UsageBucket | undefined {
  const current = text(record(value?.rateLimits).limitId);
  return buckets.find(bucket => current && bucket.id === current) ?? buckets.find(bucket => bucket.id === 'codex') ?? buckets.find(bucket => bucket.windows.length);
}

const gib = (bytes: number) => (bytes / 1073741824).toFixed(bytes >= 107374182400 ? 0 : 1) + ' GiB';
const share = (used: number, total: number) => total > 0 ? Math.min(100, Math.max(0, Math.round(used / total * 100))) : null;
/** Values older than three sampling periods are no longer presented as current. */
export const staleAfterMs = (metrics: Agent.HostMetrics) => Math.max(60, metrics.sampleSeconds) * 3000;
export const metricsStale = (metrics: Agent.HostMetrics, now = Date.now()) => now - Date.parse(metrics.observedAt) > staleAfterMs(metrics);

export function systemMeters(metrics: Agent.HostMetrics): UsageMeter[] {
  const meters: UsageMeter[] = [];
  meters.push(metrics.memory
    ? { id: 'memory', caption: 'RAM', label: 'Memory', used: share(metrics.memory.usedBytes, metrics.memory.totalBytes),
      detail: gib(metrics.memory.usedBytes) + ' of ' + gib(metrics.memory.totalBytes) }
    : { id: 'memory', caption: 'RAM', label: 'Memory', used: null, detail: 'not reported by this host' });
  const cpu = metrics.cpu, minutes = cpu ? Math.round(cpu.averageSeconds / 60) : 0;
  meters.push(!cpu
    ? { id: 'cpu', caption: 'CPU', label: 'CPU', used: null, detail: 'not reported by this host' }
    : cpu.usedPercent === null
      ? { id: 'cpu', caption: 'CPU', label: 'CPU', used: null, detail: 'measuring the first minute' }
      : { id: 'cpu', caption: 'CPU', label: 'CPU', used: Math.round(cpu.usedPercent),
        detail: cpu.coveredSeconds < cpu.averageSeconds
          ? 'average of the last ' + Math.max(1, Math.round(cpu.coveredSeconds / 60)) + ' min (warming up to ' + minutes + ' min)'
          : minutes + '-min average of all cores' });
  if (!metrics.volumes.length) meters.push({ id: 'disk', caption: 'Disk', label: 'Project disk', used: null, detail: 'project folder not available' });
  metrics.volumes.forEach((volume, index) => meters.push({ id: 'disk:' + index, caption: 'Disk', label: 'Project disk',
    used: share(volume.usedBytes, volume.totalBytes),
    detail: gib(volume.usedBytes) + ' of ' + gib(volume.totalBytes) + ' · ' + (volume.volume ? volume.volume + ' · ' : '') + volume.path }));
  return meters;
}

/** One group per account; an unready or unreported account stays visible as unknown. */
export function accountGroups(accounts: AccountUsage[]): UsageMeterGroup[] {
  const names = accounts.map(account => account.label.split(' · ')[0]!);
  return accounts.map((account, index) => {
    // Provider names suffice unless one provider has several accounts on the host.
    const caption = names.filter(name => name === names[index]).length > 1 ? account.label.split(' (')[0]!.replace(' · ', ' ') : names[index]!;
    const buckets = usageBuckets(account.result), core = coreBucket(account.result, buckets);
    const blocked = account.result?.ordinaryUsageAllowed === false;
    const meters: UsageMeter[] = core?.windows.length
      ? core.windows.map(window => ({ id: window.label, caption: window.short, label: account.label + ' · ' + window.label,
        used: blocked ? 100 : window.used, detail: [window.resets, blocked ? 'included usage unavailable' : ''].filter(Boolean).join(' · ') || undefined }))
      : [{ id: 'unknown', caption: '', label: account.label, used: blocked ? 100 : null,
        detail: !account.ready ? 'account not ready' : account.error ? 'usage unavailable' : !account.result ? 'loading'
          : Object.keys(record(account.result.rateLimitsByLimitId)).length || text(record(account.result.rateLimits).limitId) ? 'no reading yet' : 'no limit reported' }];
    return { id: account.key, caption, meters };
  });
}

/** Visible tabs of one browser share a recent host observation instead of each asking the host. */
export async function sharedRead<T>(key: string, maxAgeMs: number, load: () => Promise<T>, force = false): Promise<T> {
  const name = 'ivy.shared-read:' + key;
  if (!force) try {
    const stored = JSON.parse(localStorage.getItem(name) ?? 'null') as { at?: unknown; value?: T } | null;
    const age = typeof stored?.at === 'number' ? Date.now() - stored.at : -1;
    if (stored && age >= 0 && age < maxAgeMs) return stored.value as T;
  } catch { /* Optional shared cache. */ }
  const value = await load();
  try { localStorage.setItem(name, JSON.stringify({ at: Date.now(), value })); } catch { /* Optional shared cache. */ }
  return value;
}
