<script setup lang="ts">
import { ref } from "vue";
import ExecutionDetails from "./ExecutionDetails.vue";
import {
  Button,
  PageControls,
  PageHeader,
  RemoteState,
  StatusBadge,
} from "@ivy/ui";
import { CalendarClock, Zap } from "@lucide/vue";
import { dateLabel, listSummaries, summaryPageKey, route, tr } from "./runtime";
import type { ExecutionSummary, Pin } from "./types";
import { usePage } from "../../../packages/ui-client/src/runtime";
const props = defineProps<{ root: string }>();
const rows = usePage(
  (signal, cursor) =>
    listSummaries("secretary/execution", props.root, signal, 50, cursor),
  5000,
  () => summaryPageKey("secretary/execution", props.root),
  ["objects/secretary/execution"],
);
const selected = ref<Pin | null>(null);
const tone = (phase: string) =>
  phase === "failed"
    ? ("bad" as const)
    : ["completed", "settled", "archived", "deleted"].includes(phase)
      ? ("good" as const)
      : ["running", "starting", "archiving", "deleting"].includes(phase)
        ? ("warning" as const)
        : ("neutral" as const);
const phaseLabel = (phase: string) =>
  ({
    queued: tr("Wartet", "Queued"),
    preflight: tr("Vorprüfung", "Preflight"),
    starting: tr("Startet", "Starting"),
    running: tr("Läuft", "Running"),
    completed: tr("Abgeschlossen", "Completed"),
    settled: tr("Abgeschlossen", "Completed"),
    archiving: tr("Wird archiviert", "Archiving"),
    archived: tr("Archiviert", "Archived"),
    deleting: tr("Wird gelöscht", "Deleting"),
    deleted: tr("Task gelöscht", "Task deleted"),
    failed: tr("Fehlgeschlagen", "Failed"),
  })[phase] ?? phase;
const cleanupLabel = (execution: ExecutionSummary) => {
  if (execution.cleanupState)
    return (
      tr("Bereinigung", "Cleanup") +
      ": " +
      ({
        pending: tr("Ausstehend", "Pending"),
        blocked: tr("Blockiert", "Blocked"),
        outcome_unknown: tr("Ausgang unklar", "Outcome unknown"),
        confirmed: tr("Bestätigt", "Confirmed"),
      }[execution.cleanupState] ?? execution.cleanupState) +
      (execution.cleanupError ? " · " + execution.cleanupError : "")
    );
  return ["archiving", "archived", "deleting", "deleted"].includes(
    execution.phase,
  )
    ? phaseLabel(execution.phase)
    : "";
};
const isCatchUp = (execution: ExecutionSummary) =>
  Date.parse(execution.createdAt) - Date.parse(execution.occurredAt) > 60_000;
const deliveryLabel = (state: string) =>
  ({
    queued: tr("Wartet", "Queued"),
    dispatching: tr("Wird zugestellt", "Dispatching"),
    confirmed: tr("Zugestellt", "Confirmed"),
    suppressed: tr("Unterdrückt", "Suppressed"),
    failed: tr("Fehlgeschlagen", "Failed"),
    outcome_unknown: tr("Ausgang unklar", "Outcome unknown"),
    fallback: tr("Weiter an Main", "Main fallback"),
  })[state] ?? state;
</script>
<template>
  <PageHeader
    :title="tr('Journal', 'Journal')"
    :loading="rows.loading.value"
    :updated-at="rows.updatedAt.value"
  />
  <section class="mx-auto w-full max-w-4xl space-y-4">
    <RemoteState
      :loading="rows.loading.value"
      :error="rows.error.value"
      :has-data="!!rows.value.value"
      :empty="rows.value.value?.items.length === 0"
      :empty-title="tr('Noch keine Ausführung', 'No executions yet')"
      :empty-detail="
        tr(
          'Aktivierte Aufträge erscheinen hier nach ihrem ersten Trigger.',
          'Enabled assignments appear here after their first trigger.',
        )
      "
      @retry="rows.refresh"
    />
    <Button
      v-if="rows.value.value?.items.length === 0"
      as-child
      variant="outline"
      ><a :href="route('assignments')">{{
        tr("Möglichkeiten ansehen", "Explore what Secretary can do")
      }}</a></Button
    >
    <ol
      v-if="rows.value.value?.items.length"
      class="divide-y overflow-hidden rounded-lg border bg-card"
    >
      <li
        v-for="row in rows.value.value.items"
        :key="row.pin.objectId"
        class="space-y-2 px-4 py-3"
      >
        <div class="flex flex-wrap items-start gap-3">
          <component
            :is="row.value.triggerKind === 'schedule' ? CalendarClock : Zap"
            class="mt-0.5 size-4 shrink-0 text-muted-foreground"
            :aria-label="
              row.value.triggerKind === 'schedule'
                ? tr('Zeitplan', 'Schedule')
                : tr('Ereignis', 'Event')
            "
          />
          <div class="min-w-0 flex-1">
            <h3 class="text-sm font-medium break-words">
              {{ row.value.name }}
            </h3>
            <p class="text-xs text-muted-foreground">
              {{ dateLabel(row.value.occurredAt) }}
              <template v-if="row.value.completedAt">
                · {{ tr("Beendet", "Completed") }}
                {{ dateLabel(row.value.completedAt) }}</template
              >
            </p>
          </div>
          <StatusBadge
            v-if="isCatchUp(row.value)"
            :label="tr('Nachgeholt', 'Caught up')"
            tone="warning"
          />
          <StatusBadge
            :label="phaseLabel(row.value.phase)"
            :tone="tone(row.value.phase)"
          />
        </div>
        <div
          v-if="
            row.value.deliveryState ||
            row.value.voiceState ||
            cleanupLabel(row.value)
          "
          class="flex flex-wrap gap-2 text-xs text-muted-foreground"
        >
          <span v-if="row.value.deliveryState" class="break-words"
            >Main: {{ deliveryLabel(row.value.deliveryState)
            }}<template v-if="row.value.deliveryReason">
              · {{ row.value.deliveryReason }}</template
            ></span
          >
          <span v-if="cleanupLabel(row.value)" class="break-words">{{
            cleanupLabel(row.value)
          }}</span>
          <span v-if="row.value.voiceState" class="break-words"
            >Voice: {{ deliveryLabel(row.value.voiceState)
            }}<template v-if="row.value.voiceReason">
              · {{ row.value.voiceReason }}</template
            ></span
          >
        </div>
        <p v-if="row.value.errorCode" class="pl-7 text-sm text-destructive">
          {{ row.value.errorCode }}
        </p>
        <Button size="sm" variant="ghost" @click="selected = { ...row.pin }">{{
          tr("Details ansehen", "View details")
        }}</Button>
      </li>
    </ol>
    <PageControls
      v-if="rows.value.value?.nextCursor || rows.page.value > 1"
      :page="rows.page.value"
      :count="rows.value.value?.items.length ?? 0"
      :has-next="!!rows.value.value?.nextCursor"
      :loading="rows.loading.value"
      @previous="rows.previous"
      @next="rows.next"
    />
    <p
      v-if="rows.value.value?.items.length"
      class="text-xs text-muted-foreground"
    >
      {{
        tr(
          "Fertige Tasks werden archiviert und nach sieben Tagen gelöscht; das Journal bleibt erhalten.",
          "Finished tasks are archived and deleted after seven days; the journal remains.",
        )
      }}
    </p>
  </section>
  <ExecutionDetails :pin="selected" :root="root" @close="selected = null" />
</template>
