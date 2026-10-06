import definition from "../ui.json";
import { uiRuntime } from "../../../packages/ui-client/src/runtime";
import { nativeOutputCache } from "../../../packages/ui-client/src/native-output-cache";
import { record, text } from "../../../packages/ui-client/src/native";
export const { base, client, uiUrl, notifications, live } = uiRuntime(
  definition.metadata,
);
export const outputCache = nativeOutputCache(client);
// Invalidate inactive conversations too; cached previews never survive a known history change.
notifications.subscribe(
  { namespace: "agent", name: "notification", version: "1.0.0" },
  (value) => {
    const event = record(value.params.payload),
      params = record(event.params);
    if (text(event.epoch))
      outputCache.observeEpoch(value.params.serviceNodeId, text(event.epoch));
    if (
      [
        "thread/deleted",
        "thread/reverted",
        "thread/compacted",
        "item/completed",
        "turn/completed",
      ].includes(text(event.method))
    )
      outputCache.invalidate(
        value.params.serviceNodeId,
        text(params.threadId) || undefined,
      );
  },
);
notifications.subscribeChanges(["services"], () => outputCache.invalidate());
