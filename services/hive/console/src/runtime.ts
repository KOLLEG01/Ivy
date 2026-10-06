import { ref } from 'vue';
import { usePagePosition, useRemote } from '@ivy/ui';
import { browserClient, browserNotifications } from '../../../../packages/sdk/src/client.js';
import { liveUpdates } from '../../../../packages/ui-client/src/live';
import type { Operation } from '../../../../packages/sdk/src/client.js';
import { uiPath } from '../../../../packages/contracts/src/ui-route.js';
export type { Operation } from '../../../../packages/sdk/src/client.js';

export const consoleBase = new URL(location.pathname.endsWith('/console/index.html') ? '../../' : './', location.href);
export const consoleClient = browserClient(consoleBase.href);
export const live = liveUpdates(browserNotifications(consoleBase.href));
export function navigate(section: string, values: Record<string, string> = {}) {
  const query = new URLSearchParams(Object.entries(values).filter(([, value]) => value.length));
  location.hash = '/system/' + section + (query.size ? '?' + query.toString() : '');
}
export const uiUrl = (ui: { metadata: Operation.UiMetadata }) => new URL(uiPath(ui.metadata), consoleBase).href;
export const dateLabel = (value: string | null | undefined) => value ? new Date(value).toLocaleString() : 'Not observed';
export const shortHash = (value: string) => value.replace(/^sha256:/, '').slice(0, 12);
export function usePage<R extends { items: unknown[]; nextCursor: string | null }>(load: (signal: AbortSignal, cursor: string | undefined) => Promise<R>, storageKey?: () => string, live?: readonly string[]) {
  const { cursors, page, remember, restore: restorePosition } = usePagePosition(storageKey);
  const remote = useRemote(signal => load(signal, cursors.value[page.value - 1]), 0, live);
  const next = () => { if (!remote.value.value?.nextCursor || remote.loading.value) return;
    cursors.value[page.value] = remote.value.value.nextCursor; page.value++; remember(); void remote.refresh(); };
  const previous = () => { if (page.value <= 1 || remote.loading.value) return; page.value--; remember(); void remote.refresh(); };
  const reset = () => { cursors.value = [undefined]; page.value = 1; remember(); void remote.refresh(); };
  const restore = () => { restorePosition(); void remote.refresh(); };
  return { ...remote, page, next, previous, reset, restore };
}
