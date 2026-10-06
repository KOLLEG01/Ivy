<script setup lang="ts">
import { Button, PageControls, PageHeader, RemoteState, StatusBadge } from "@ivy/ui";
import { CalendarClock, ExternalLink, Zap } from "@lucide/vue";
import { agentTaskUrl, dateLabel, listDocuments, route, tr } from "./runtime";
import type { Execution } from "./types";
import { usePage } from "../../../packages/ui-client/src/runtime";
const props = defineProps<{ root: string }>();
const rows = usePage(
  (signal, cursor) =>
    listDocuments("secretary/execution", props.root, signal, 50, cursor),
  5000,
  () => "secretary-journal:" + props.root,
  ["objects/secretary/execution"],
);
const tone = (phase: string) =>
  phase === "failed"
    ? ("bad" as const)
    : ["archived", "deleted"].includes(phase)
      ? ("good" as const)
      : ["running", "starting", "archiving", "deleting"].includes(
            phase,
          )
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
const resultText = (result: string) => {
  try { const value = JSON.parse(result); return typeof value?.text === "string" ? value.text || value.reason : result; }
  catch { return result; }
};
const isCatchUp = (execution: Execution) =>
  Date.parse(execution.createdAt) - Date.parse(execution.trigger.occurredAt) > 60_000;
const deliveryLabel = (state: string) => ({
  queued: tr("Wartet", "Queued"), dispatching: tr("Wird zugestellt", "Dispatching"),
  confirmed: tr("Zugestellt", "Confirmed"), suppressed: tr("Unterdrückt", "Suppressed"),
  failed: tr("Fehlgeschlagen", "Failed"), outcome_unknown: tr("Ausgang unklar", "Outcome unknown"),
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
    <Button v-if="rows.value.value?.items.length === 0" as-child variant="outline"><a :href="route('assignments')">{{ tr("Möglichkeiten ansehen", "Explore what Secretary can do") }}</a></Button>
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
            :is="row.value.trigger.kind === 'schedule' ? CalendarClock : Zap"
            class="mt-0.5 size-4 shrink-0 text-muted-foreground"
            :aria-label="
              row.value.trigger.kind === 'schedule'
                ? tr('Zeitplan', 'Schedule')
                : tr('Ereignis', 'Event')
            "
          />
          <div class="min-w-0 flex-1">
            <h3 class="text-sm font-medium break-words">
              {{ row.value.assignmentSnapshot.name }}
            </h3>
            <p class="text-xs text-muted-foreground">
              {{ dateLabel(row.value.trigger.occurredAt) }}
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
          v-if="row.value.delivery || row.value.voice"
          class="flex flex-wrap gap-2 text-xs text-muted-foreground"
        >
          <span v-if="row.value.delivery" class="break-words"
            >Main: {{ deliveryLabel(row.value.delivery.state)
            }}<template v-if="row.value.delivery.reason">
              · {{ row.value.delivery.reason }}</template
            ></span
          >
          <span v-if="row.value.voice" class="break-words"
            >Voice: {{ deliveryLabel(row.value.voice.state)
            }}<template v-if="row.value.voice.reason">
              · {{ row.value.voice.reason }}</template
            ></span
          >
        </div>
        <p
          v-if="row.value.result"
          class="line-clamp-3 pl-7 text-sm whitespace-pre-wrap text-muted-foreground"
          :title="resultText(row.value.result)"
        >
          {{ resultText(row.value.result) }}
        </p>
        <p v-if="row.value.errorCode" class="pl-7 text-sm text-destructive">
          {{ row.value.errorCode }}
        </p>
        <p
          v-if="row.value.errorCode && row.value.preflightOutput"
          class="line-clamp-3 pl-7 text-sm whitespace-pre-wrap text-muted-foreground"
        >
          {{ row.value.preflightOutput }}
        </p>
        <div
          v-if="
            row.value.phase !== 'deleted' &&
            (row.value.threadId || row.value.deleteAfter)
          "
          class="flex flex-wrap items-center gap-3 pl-7 text-xs text-muted-foreground"
        >
          <a
            v-if="row.value.threadId"
            class="inline-flex items-center gap-1 font-medium text-foreground hover:underline"
            :href="agentTaskUrl(row.value.serviceNodeId, row.value.threadId)"
            >{{ tr("Codex-Task öffnen", "Open Codex task")
            }}<ExternalLink class="size-3" aria-hidden="true"
          /></a>
          <span v-if="row.value.deleteAfter"
            >{{ tr("Task-Löschung", "Task deletion") }}
            {{ dateLabel(row.value.deleteAfter) }}</span
          >
        </div>
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
</template>
