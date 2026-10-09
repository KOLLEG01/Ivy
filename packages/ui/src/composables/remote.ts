import { inject, onBeforeUnmount, onMounted, ref, shallowRef, watch } from "vue";
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
export type RemoteSource = readonly string[] | (() => readonly string[]) | RemoteUpdates;

export function useRemote<T>(
  loader: (signal: AbortSignal) => Promise<T>,
  intervalMs = 0,
  live?: RemoteSource,
) {
  // A connection only replaces polling when this loader has an explicit source.
  const updates =
    live && typeof live === "object" && "subscribe" in live
      ? live
      : typeof live === "function" || live?.length
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
  let started = false;
  let timer: ReturnType<typeof setInterval> | undefined,
    queued: ReturnType<typeof setTimeout> | undefined,
    unsubscribe: (() => void) | undefined;
  let initial: ReturnType<typeof setTimeout> | undefined;
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
    clearTimeout(initial);
    started = true;
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
    const subscribe = () => {
      if (!updates) return;
      unsubscribe?.();
      unsubscribe = updates.subscribe(
        schedule,
        (ready) => {
          const recovered = ready && !connected;
          connected = ready;
          if (recovered && !started) void refresh();
          else if (recovered) schedule();
        },
        typeof live === "function" ? live() : Array.isArray(live) ? live : undefined,
      );
    };
    if (updates) {
      subscribe();
      if (typeof live === "function")
        watch(
          () => live().join("\u0000"),
          () => {
            subscribe();
            schedule();
          },
        );
    }
    // Read after subscribing so the first snapshot already covers the live stream.
    // An unavailable socket may delay the initial read by at most 250 ms.
    if (!started) {
      if (updates)
        initial = setTimeout(() => {
          if (!started) void refresh();
        }, 250);
      else void refresh();
    }
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
    clearTimeout(initial);
    unsubscribe?.();
    window.removeEventListener("online", online);
    document.removeEventListener("visibilitychange", visible);
  });
  return { value, loading, error, updatedAt, refresh };
}
