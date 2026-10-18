import { computed, ref, watch } from "vue";
import { base, tr } from "./runtime";
import type { TaskBoard } from "./runtime";

export interface CommentPreferences {
  order: "newest-first" | "oldest-first";
  collapseOlder: boolean;
}

export function useCommentPreferences(
  workspace: () => TaskBoard.WorkspaceInfo,
) {
  const key = computed(() => {
    const value = workspace();
    return (
      "ivy.task-board.comments:" +
      JSON.stringify([
        base.href,
        value.serviceNodeId,
        value.callerPrincipalId,
        value.principalId,
        value.rootObjectId,
      ])
    );
  });
  // The conversation reads like a chat by default: oldest first, newest answer next to the input.
  const preferences = ref<CommentPreferences>({
    order: "oldest-first",
    collapseOlder: true,
  });
  const storageError = ref("");
  watch(
    key,
    () => {
      try {
        const stored = JSON.parse(localStorage.getItem(key.value) ?? "null");
        preferences.value = {
          order:
            stored?.order === "newest-first" ? "newest-first" : "oldest-first",
          collapseOlder:
            typeof stored?.collapseOlder === "boolean"
              ? stored.collapseOlder
              : true,
        };
      } catch {
        storageError.value = tr(
          "Die Anzeigeeinstellungen konnten nicht gelesen werden.",
          "Display settings could not be read.",
        );
      }
    },
    { immediate: true },
  );
  watch(
    preferences,
    (value) => {
      try {
        localStorage.setItem(key.value, JSON.stringify(value));
        storageError.value = "";
      } catch {
        storageError.value = tr(
          "Die Anzeigeeinstellungen konnten nicht gespeichert werden.",
          "Display settings could not be saved.",
        );
      }
    },
    { deep: true },
  );
  return { preferences, storageError };
}
