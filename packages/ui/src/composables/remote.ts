import { inject, onBeforeUnmount, onMounted, ref, shallowRef } from "vue";
import type { InjectionKey } from "vue";

/** Applications supply their transport; components only know invalidation and readiness. */
export interface RemoteUpdates {
  subscribe(
    changed: () => void,
    status: (ready: boolean) => void,
    scopes?: readonly string[],
  ): () => void;
}
export const remoteUpdatesKey: InjectionKey<RemoteUpdates> =
  Symbol("remote-updates");
export type RemoteSource = readonly string[] | RemoteUpdates;

export function useRemote<T>(
  loader: (signal: AbortSignal) => Promise<T>,
  intervalMs = 0,
  live?: RemoteSource,
) {
  // A connection only replaces polling when this loader has an explicit source.
  const updates =
    live && "subscribe" in live
      ? live
      : live?.length
        ? inject(remoteUpdatesKey, null)
        : null;
  const value = shallowRef<T | null>(null),
    loading = ref(false),
    error = ref<string | null>(null),
    updatedAt = ref<string | null>(null);
  let controller: AbortController | null = null,
    serial = 0,
    stopped = false,
    connected = false,
    dirty = false;
  let timer: ReturnType<typeof setInterval> | undefined,
    queued: ReturnType<typeof setTimeout> | undefined,
    unsubscribe: (() => void) | undefined;
  // Coalesce bursts; retain a hint arriving during a read for one follow-up.
  const schedule = () => {
    dirty = true;
    if (stopped || document.hidden || loading.value || queued) return;
    queued = setTimeout(() => {
      queued = undefined;
      if (!stopped && !document.hidden && dirty && !loading.value)
        void refresh();
    }, 250);
  };
  const refresh = async () => {
    if (stopped) return;
    clearTimeout(queued);
    queued = undefined;
    dirty = false;
    controller?.abort();
    controller = new AbortController();
    const current = ++serial,
      signal = controller.signal;
    loading.value = true;
    try {
      const next = await loader(signal);
      if (current !== serial || signal.aborted) return;
      value.value = next;
      updatedAt.value = new Date().toISOString();
      error.value = null;
    } catch (cause) {
      if (current !== serial || signal.aborted) return;
      error.value =
        cause instanceof Error
          ? cause.message
          : "The request could not be completed.";
    } finally {
      if (current === serial) {
        loading.value = false;
        if (dirty) schedule();
      }
    }
  };
  const online = () => {
    schedule();
  };
  const visible = () => {
    if (!document.hidden) schedule();
  };
  onMounted(() => {
    if (updates)
      unsubscribe = updates.subscribe(
        schedule,
        (ready) => {
          const recovered = ready && !connected;
          connected = ready;
          if (recovered) schedule();
        },
        Array.isArray(live) ? live : undefined,
      );
    void refresh();
    if (updates || intervalMs) {
      window.addEventListener("online", online);
      document.addEventListener("visibilitychange", visible);
      timer = setInterval(
        () => {
          if (
            !document.hidden &&
            !loading.value &&
            (!updates || !connected || error.value)
          )
            schedule();
        },
        Math.max(15000, intervalMs || 15000),
      );
    }
  });
  onBeforeUnmount(() => {
    stopped = true;
    serial++;
    controller?.abort();
    clearInterval(timer);
    clearTimeout(queued);
    unsubscribe?.();
    window.removeEventListener("online", online);
    document.removeEventListener("visibilitychange", visible);
  });
  return { value, loading, error, updatedAt, refresh };
}
