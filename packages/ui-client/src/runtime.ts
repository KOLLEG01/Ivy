import { onBeforeUnmount, ref } from 'vue';
import { usePagePosition, useRemote } from '@ivy/ui';
import type { RemoteSource } from '@ivy/ui';
import { browserClient, browserNotifications } from '../../sdk/src/client.js';
import { liveUpdates } from './live';
import { uiPath } from '../../contracts/src/ui-route.js';
import type { Operation } from '../../sdk/src/client.js';
export { IvyError } from '../../sdk/src/client.js';
export type { Operation } from '../../sdk/src/client.js';

export function uiRuntime(metadata: Pick<Operation.UiMetadata, 'uiId' | 'slug'>) {
  const suffixes = ['/ui/' + encodeURIComponent(metadata.uiId) + '/', ...(metadata.slug ? ['/' + metadata.slug + '/'] : [])];
  const suffix = suffixes.find(value => location.pathname.endsWith(value) || location.pathname.endsWith(value + 'index.html')) ?? suffixes[0]!;
  const index = location.pathname.lastIndexOf(suffix);
  if (index < 0 || !/^(?:index\.html)?$/.test(location.pathname.slice(index + suffix.length))) throw new Error('Open this UI through its Hive URL.');
  const base = new URL(location.pathname.slice(0, index) + '/', location.origin);
  const notifications = browserNotifications(base.href);
  return { base, client: browserClient(base.href), notifications, live: liveUpdates(notifications), uiUrl: new URL(uiPath(metadata), base).href };
}
export function useHashRoute() {
  const hash = ref(location.hash || '#/home');
  const changed = () => { hash.value = location.hash || '#/home'; };
  window.addEventListener('hashchange', changed); onBeforeUnmount(() => window.removeEventListener('hashchange', changed));
  return hash;
}
export function route(section: string, values: Record<string, string> = {}) {
  const query = new URLSearchParams(Object.entries(values).filter(([, value]) => value));
  return '#/' + section + (query.size ? '?' + query.toString() : '');
}
export function usePage<R extends { items: unknown[]; nextCursor: string | null }>(load: (signal: AbortSignal, cursor: string | undefined) => Promise<R>, intervalMs = 0, storageKey?: () => string, live?: RemoteSource) {
  const { cursors, page, remember } = usePagePosition(storageKey);
  const remote = useRemote(signal => load(signal, cursors.value[page.value - 1]), intervalMs, live);
  const next = () => { if (remote.loading.value || !remote.value.value?.nextCursor) return; cursors.value[page.value] = remote.value.value.nextCursor; page.value++; remember(); void remote.refresh(); };
  const previous = () => { if (remote.loading.value || page.value <= 1) return; page.value--; remember(); void remote.refresh(); };
  const reset = () => { cursors.value = [undefined]; page.value = 1; remember(); void remote.refresh(); };
  return { ...remote, page, next, previous, reset };
}
export const dateLabel = (value: string) => new Date(value).toLocaleString();
