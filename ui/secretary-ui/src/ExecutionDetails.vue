<script setup lang="ts">
import { watch } from "vue";
import {
  RemoteState,
  Sheet,
  SheetContent,
  SheetDescription,
  SheetHeader,
  SheetTitle,
  StatusBadge,
  useRemote,
} from "@ivy/ui";
import { ExternalLink } from "@lucide/vue";
import { agentTaskUrl, dateLabel, readDocument, tr } from "./runtime";
import type { Pin } from "./types";
const props = defineProps<{ pin: Pin | null; root: string }>();
const emit = defineEmits<{ close: [] }>();
const full = useRemote(
  async (signal) => {
    if (!props.pin) throw new Error("Choose an execution.");
    return readDocument(
      "secretary/execution",
      { ...props.pin },
      props.root,
      signal,
    );
  },
  0,
  undefined,
  () => !!props.pin,
);
watch(
  () => props.pin,
  () => {
    full.value.value = null;
    if (props.pin) void full.refresh();
  },
);
const resultText = (result: string) => {
  try {
    const value = JSON.parse(result);
    return typeof value?.text === "string"
      ? value.text || value.reason
      : result;
  } catch {
    return result;
  }
};
</script>
<template>
  <Sheet :open="!!pin" @update:open="!$event && emit('close')">
    <SheetContent class="w-full overflow-y-auto sm:max-w-xl">
      <SheetHeader>
        <SheetTitle>{{
          full.value.value?.value.assignmentSnapshot.name ??
          tr("Ausführung", "Execution")
        }}</SheetTitle>
        <SheetDescription>{{
          tr(
            "Gespeicherte Details dieser Ausführungsrevision.",
            "Saved details of this execution revision.",
          )
        }}</SheetDescription>
      </SheetHeader>
      <RemoteState
        :loading="full.loading.value"
        :error="full.error.value"
        :has-data="!!full.value.value"
        @retry="full.refresh"
      />
      <div v-if="full.value.value" class="space-y-4 p-4 text-sm">
        <StatusBadge
          v-if="full.value.value.value.completedAt"
          :label="
            full.value.value.value.result !== null
              ? tr('Abgeschlossen', 'Completed')
              : tr('Fehlgeschlagen', 'Failed')
          "
          :tone="full.value.value.value.result !== null ? 'good' : 'bad'"
        />
        <p
          v-if="full.value.value.value.result"
          class="whitespace-pre-wrap break-words"
        >
          {{ resultText(full.value.value.value.result) }}
        </p>
        <p v-if="full.value.value.value.errorCode" class="text-destructive">
          {{ full.value.value.value.errorCode }}
        </p>
        <p
          v-if="
            full.value.value.value.errorCode &&
            full.value.value.value.preflightOutput
          "
          class="whitespace-pre-wrap break-words"
        >
          {{ full.value.value.value.preflightOutput }}
        </p>
        <p
          v-if="
            !full.value.value.value.result && !full.value.value.value.errorCode
          "
          class="text-muted-foreground"
        >
          {{ tr("Noch kein Ergebnis gespeichert.", "No result saved yet.") }}
        </p>
        <div
          v-if="full.value.value.value.phase !== 'deleted'"
          class="flex flex-wrap gap-3 text-xs text-muted-foreground"
        >
          <a
            v-if="full.value.value.value.threadId"
            class="inline-flex items-center gap-1 font-medium text-foreground hover:underline"
            :href="
              agentTaskUrl(
                full.value.value.value.serviceNodeId,
                full.value.value.value.threadId,
                full.value.value.value.nativeTarget?.backend,
              )
            "
          >
            {{ tr("Agent-Task öffnen", "Open agent task")
            }}<ExternalLink class="size-3" aria-hidden="true" />
          </a>
          <span v-if="full.value.value.value.deleteAfter"
            >{{ tr("Task-Löschung", "Task deletion") }}
            {{ dateLabel(full.value.value.value.deleteAfter) }}</span
          >
        </div>
      </div>
    </SheetContent>
  </Sheet>
</template>
