<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref, shallowRef, watch } from "vue";
import { CircleAlert, Pencil, Play } from "@lucide/vue";
import {
  Alert,
  AlertDescription,
  AlertTitle,
  Button,
  Card,
  CardContent,
  CardHeader,
  CardTitle,
  Empty,
  EmptyDescription,
  EmptyHeader,
  EmptyTitle,
  ImagePreview,
  JsonTree,
  OptionSelect,
  PropertyItem,
  PropertyList,
  StatusBadge,
  Switch,
  ToolbarContent,
} from "@ivy/ui";
import type { Operation } from "../../../packages/sdk/src/client.js";
import {
  intervalLabel,
  read,
  statusLabel,
  statusTone,
  taskError,
  tr,
  working,
} from "./collector";
import type { Task, TaskRow } from "./collector";

const props = defineProps<{ node: string; row: TaskRow; busy: boolean }>();
const emit = defineEmits<{
  edit: [];
  run: [];
  toggle: [];
  activate: [revision: number];
}>();
const definitionHistory = ref<Operation.RevisionMetadata[]>([]),
  selectedDefinition = ref("");
const task = ref<Task | null>(null),
  result = shallowRef<Operation.ObjectRead | null>(null),
  history = ref<Operation.RevisionMetadata[]>([]),
  cursor = ref<string | null>(null),
  revision = ref(""),
  resultLoading = ref(false),
  historyLoading = ref(false),
  error = ref(""),
  enlarged = ref<{ src: string; name: string } | null>(null);
const loading = computed(() => resultLoading.value || historyLoading.value);
const id = () => props.row.task.id;
let alive = true,
  definitionRequest = 0,
  resultRequest = 0,
  historyRequest = 0;
onUnmounted(() => {
  alive = false;
});
const failure = (cause: unknown) => {
  if (alive)
    error.value = cause instanceof Error ? cause.message : String(cause);
};
async function loadDefinition() {
  const request = ++definitionRequest;
  try {
    const [full, definitions] = await Promise.all([
      read<{ task: Task }>(props.node, { view: "task", id: id() }),
      props.row.jobId
        ? read<Operation.ObjectsHistoryResult>(props.node, {
            view: "definitionHistory",
            jobId: props.row.jobId,
          })
        : null,
    ]);
    if (!alive || request !== definitionRequest) return;
    task.value = full.task;
    definitionHistory.value = definitions?.items ?? [];
  } catch (cause) {
    if (request === definitionRequest) failure(cause);
  }
}
async function loadResult() {
  const request = ++resultRequest;
  resultLoading.value = true;
  try {
    const value = await read<Operation.ObjectRead | null>(props.node, {
      view: "result",
      id: id(),
      ...(revision.value ? { revision: Number(revision.value) } : {}),
    });
    if (alive && request === resultRequest) result.value = value;
  } catch (cause) {
    if (request === resultRequest) failure(cause);
  } finally {
    if (alive && request === resultRequest) resultLoading.value = false;
  }
}
async function loadHistory(more = false) {
  const request = ++historyRequest;
  historyLoading.value = true;
  try {
    const page = await read<Operation.ObjectsHistoryResult | null>(props.node, {
      view: "history",
      id: id(),
      ...(more && cursor.value ? { cursor: cursor.value } : {}),
    });
    if (!alive || request !== historyRequest) return;
    history.value = more
      ? [...history.value, ...(page?.items ?? [])]
      : (page?.items ?? []);
    cursor.value = page?.nextCursor ?? null;
  } catch (cause) {
    if (request === historyRequest) failure(cause);
  } finally {
    if (alive && request === historyRequest) historyLoading.value = false;
  }
}
async function load(more = false) {
  error.value = "";
  if (more) return loadHistory(true);
  await Promise.all([loadDefinition(), loadResult(), loadHistory()]);
}
onMounted(() => void load());
watch(
  () => props.row.revision,
  () => void loadDefinition(),
);
watch(revision, () => void loadResult());
watch([() => props.row.resultObjectId, () => props.row.resultRevision], () => {
  if (!revision.value) void loadResult();
  void loadHistory();
});
const images = computed(() => {
  const value =
    result.value?.content.encoding === "json"
      ? result.value.content.value
      : null;
  const data =
    value && typeof value === "object" && !Array.isArray(value)
      ? value["data"]
      : null;
  const list =
    data && typeof data === "object" && !Array.isArray(data)
      ? data["images"]
      : null;
  return Array.isArray(list)
    ? list.flatMap((item) =>
        item &&
        typeof item === "object" &&
        !Array.isArray(item) &&
        typeof item["dataUrl"] === "string" &&
        /^data:image\/(jpeg|png|webp);base64,[A-Za-z0-9+/=]+$/.test(
          item["dataUrl"],
        )
          ? [
              {
                src: item["dataUrl"],
                name: String(item["name"] ?? item["id"] ?? "Camera"),
              },
            ]
          : [],
      )
    : [];
});
</script>

<template>
  <ToolbarContent side="end">
    <template v-if="row.jobId">
      <OptionSelect
        v-model="selectedDefinition"
        class="w-auto"
        :aria-label="tr('Definitionsrevision', 'Definition revision')"
        :disabled="busy"
      >
        <option value="">
          {{ tr(`Entwurf R${row.revision}`, `Draft R${row.revision}`) }}
        </option>
        <option
          v-for="entry in definitionHistory.filter(
            (r) => r.revision !== row.revision,
          )"
          :key="entry.revision"
          :value="String(entry.revision)"
        >
          R{{ entry.revision }}
        </option>
      </OptionSelect>
      <Button
        variant="outline"
        size="sm"
        :disabled="busy"
        @click="emit('activate', Number(selectedDefinition || row.revision))"
        >{{ tr("Stand übernehmen", "Apply revision") }}</Button
      >
    </template>
    <Button variant="outline" size="sm" :disabled="busy" @click="emit('edit')"
      ><Pencil aria-hidden="true" />{{ tr("Bearbeiten", "Edit") }}</Button
    >
    <Button
      size="sm"
      :disabled="
        busy || working(row.status) || (!!row.jobId && !row.adoptedRevision)
      "
      @click="emit('run')"
      ><Play aria-hidden="true" />{{ tr("Jetzt ausführen", "Run now") }}</Button
    >
  </ToolbarContent>
  <div class="mx-auto w-full max-w-5xl space-y-6">
    <header class="flex flex-wrap items-start justify-between gap-4">
      <div class="min-w-0 space-y-2">
        <h2
          class="text-2xl font-semibold tracking-tight [overflow-wrap:anywhere]"
        >
          {{ row.task.name }}
        </h2>
        <div class="flex flex-wrap items-center gap-2">
          <StatusBadge
            :label="statusLabel(row.status)"
            :tone="statusTone(row.status)"
          />
          <span class="font-mono text-xs text-muted-foreground">{{
            row.task.id
          }}</span>
        </div>
      </div>
      <label class="flex items-center gap-2 text-sm">
        <Switch
          :model-value="row.desiredEnabled ?? row.task.enabled"
          :disabled="busy"
          :aria-label="tr('Task aktiv', 'Task enabled')"
          @update:model-value="emit('toggle')"
        />
        {{
          (row.desiredEnabled ?? row.task.enabled)
            ? tr("Aktiv", "Enabled")
            : tr("Pausiert", "Disabled")
        }}
      </label>
    </header>

    <Alert v-if="row.error" variant="destructive">
      <CircleAlert aria-hidden="true" />
      <AlertTitle>{{
        tr("Letzter Lauf fehlgeschlagen", "Last run failed")
      }}</AlertTitle>
      <AlertDescription>{{ taskError(row.error) }}</AlertDescription>
    </Alert>
    <Alert v-if="row.adoptionError" variant="destructive"
      ><AlertDescription
        >{{ tr("Übernahme fehlgeschlagen: ", "Adoption failed: ")
        }}{{ row.adoptionError }}</AlertDescription
      ></Alert
    >
    <Alert v-if="row.retentionWarning"
      ><AlertDescription>{{ row.retentionWarning }}</AlertDescription></Alert
    >

    <PropertyList>
      <PropertyItem
        v-if="row.jobId"
        :label="tr('Gewünscht / übernommen', 'Desired / adopted')"
        >{{ row.desiredRevision ?? "—" }} /
        {{ row.adoptedRevision ?? "—" }}</PropertyItem
      >
      <PropertyItem
        v-if="row.pendingPublications"
        :label="tr('Ausstehende Veröffentlichungen', 'Pending publications')"
        >{{ row.pendingPublications }}</PropertyItem
      >
      <PropertyItem :label="tr('Zeitplan', 'Schedule')">{{
        task ? intervalLabel(task.intervalSeconds) : "—"
      }}</PropertyItem>
      <PropertyItem :label="tr('Letzter Start', 'Last started')">{{
        row.lastRunAt ? new Date(row.lastRunAt).toLocaleString() : "—"
      }}</PropertyItem>
      <PropertyItem :label="tr('Ergebnisse behalten', 'Keeps results')">{{
        task ? task.retention.maximumCount.toLocaleString() : "—"
      }}</PropertyItem>
    </PropertyList>

    <Card>
      <CardHeader
        class="flex flex-row flex-wrap items-center justify-between gap-3"
      >
        <CardTitle>{{ tr("Ergebnis", "Result") }}</CardTitle>
        <div class="flex flex-wrap items-center gap-2">
          <OptionSelect
            id="result-revision"
            v-model="revision"
            :aria-label="tr('Ergebnisrevision', 'Result revision')"
            :disabled="loading"
            class="w-56"
          >
            <option value="">
              {{ tr("Aktuelles Ergebnis", "Current result") }}
            </option>
            <option
              v-for="entry in history"
              :key="entry.revision"
              :value="String(entry.revision)"
            >
              #{{ entry.revision }} ·
              {{ new Date(entry.createdAt).toLocaleString() }}
            </option>
          </OptionSelect>
          <Button
            v-if="cursor"
            variant="ghost"
            size="sm"
            :disabled="loading"
            @click="load(true)"
            >{{ tr("Ältere laden", "Load older") }}</Button
          >
        </div>
      </CardHeader>
      <CardContent class="space-y-4">
        <Alert v-if="error" variant="destructive">
          <AlertDescription
            class="flex flex-wrap items-center justify-between gap-3"
          >
            <span>{{ error }}</span>
            <Button
              variant="outline"
              size="sm"
              :disabled="loading"
              @click="load()"
              >{{ tr("Erneut versuchen", "Retry") }}</Button
            >
          </AlertDescription>
        </Alert>
        <Empty v-if="!result && !loading" class="border border-dashed">
          <EmptyHeader>
            <EmptyTitle>{{
              tr("Noch kein Ergebnis", "No result yet")
            }}</EmptyTitle>
            <EmptyDescription>{{
              tr(
                "Führe den Task aus, um das erste Ergebnis zu speichern.",
                "Run the task to store its first result.",
              )
            }}</EmptyDescription>
          </EmptyHeader>
        </Empty>
        <template v-else-if="result">
          <div v-if="images.length" class="grid gap-4 sm:grid-cols-2">
            <figure v-for="image in images" :key="image.name" class="space-y-2">
              <button
                type="button"
                class="ivy-image-button"
                :aria-label="tr('Vergrößern: ', 'Enlarge ') + image.name"
                @click="enlarged = image"
              >
                <img
                  :src="image.src"
                  :alt="image.name"
                  class="ivy-embedded-image max-h-96"
                />
              </button>
              <figcaption class="text-sm text-muted-foreground">
                {{ image.name }}
              </figcaption>
            </figure>
          </div>
          <ImagePreview
            :open="!!enlarged"
            :source="enlarged?.src ?? null"
            :name="enlarged?.name ?? ''"
            @update:open="enlarged = $event ? enlarged : null"
          />
          <JsonTree
            :show-full-label="
              tr('Vollständigen Wert anzeigen', 'Show full value')
            "
            :show-preview-label="tr('Vorschau anzeigen', 'Show preview')"
            :value="result.content.value"
            :label="tr('Ergebnis', 'Result')"
            :open-depth="2"
          />
        </template>
      </CardContent>
    </Card>
  </div>
</template>
