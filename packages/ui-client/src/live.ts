import type { App } from "vue";
import { remoteUpdatesKey } from "@ivy/ui";
import type { RemoteUpdates } from "@ivy/ui";
import type { BrowserNotifications } from "../../sdk/src/client.js";

/** The SDK combines explicit loader subscriptions on one socket per app/tab. */
export function liveUpdates(notifications: BrowserNotifications) {
  const updates: RemoteUpdates = {
    subscribe(changed, status, scopes = []) {
      const stopChanges = notifications.subscribeChanges(scopes, changed);
      const stopStatus = notifications.onStatus(status);
      return () => {
        stopStatus();
        stopChanges();
      };
    },
  };
  return {
    install(app: App) {
      app.provide(remoteUpdatesKey, updates);
      const resume = () => {
        if (!document.hidden) notifications.reconnectNow();
      };
      window.addEventListener("online", resume);
      document.addEventListener("visibilitychange", resume);
      app.onUnmount(() => {
        window.removeEventListener("online", resume);
        document.removeEventListener("visibilitychange", resume);
      });
    },
  };
}

/** Pending-input notifications belong to the selected owner and task in every UI. */
export function nativeInputUpdates(
  notifications: BrowserNotifications,
  serviceNodeId: string,
  threadId: string,
): RemoteUpdates {
  return {
    subscribe(changed, status) {
      const stops = ["inputs", "notification", "capabilities"].map((name) =>
        notifications.subscribe(
          { namespace: "agent", name, version: "1.0.0", serviceNodeId },
          (frame) => {
            const payload = frame.params.payload as {
              threadId?: string;
              method?: string;
              params?: { threadId?: string };
            };
            if (
              name === "capabilities" ||
              (name === "inputs" && payload.threadId === threadId) ||
              (name === "notification" &&
                payload.method === "serverRequest/resolved" &&
                payload.params?.threadId === threadId)
            )
              changed();
          },
        ),
      );
      const stopChanges = notifications.subscribeChanges(["services"], changed);
      const stopStatus = notifications.onStatus(status);
      return () => {
        stopStatus();
        stopChanges();
        for (const stop of stops) stop();
      };
    },
  };
}
