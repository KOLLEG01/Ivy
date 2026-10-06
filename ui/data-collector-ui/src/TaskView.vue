<script setup lang="ts">
import { computed, onMounted, ref, watch } from "vue";
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
const emit = defineEmits<{ edit: []; run: []; toggle: [] }>();
const task = ref<Task | null>(null),
  result = ref<Operation.ObjectRead | null>(null),
  history = ref<Operation.RevisionMetadata[]>([]),
  cursor = ref<string | null>(null),
  revision = ref(""),
  loading = ref(false),
  error = ref(""),
  enlarged = ref<{ src: string; name: string } | null>(null);
const id = () => props.row.task.id;
async function load(more = false) {
  loading.value = true;
  error.value = "";
  try {
    if (!more)
      task.value = (
        await read<{ task: Task }>(props.node, { view: "task", id: id() })
      ).task;
    result.value = await read<Operation.ObjectRead | null>(props.node, {
      view: "result",
      id: id(),
      ...(revision.value ? { revision: Number(revision.value) } : {}),
    });
    const page = await read<Operation.ObjectsHistoryResult | null>(props.node, {
      view: "history",
      id: id(),
      ...(more && cursor.value ? { cursor: cursor.value } : {}),
    });
    history.value = more
      ? [...history.value, ...(page?.items ?? [])]
      : (page?.items ?? []);
    cursor.value = page?.nextCursor ?? null;
  } catch (cause) {
    error.value = cause instanceof Error ? cause.message : String(cause);
  } finally {
    loading.value = false;
  }
}
onMounted(() => void load());
watch(revision, () => void load());
// A finished run publishes a new result; follow it unless an older revision is pinned.
watch(
  () => [props.row.status, props.row.lastRunAt],
  ([status], previous) => {
    if (!revision.value && previous && !working(String(status))) void load();
  },
);
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
    <Button variant="outline" size="sm" :disabled="busy" @click="emit('edit')"
      ><Pencil aria-hidden="true" />{{ tr("Bearbeiten", "Edit") }}</Button
    >
    <Button
      size="sm"
      :disabled="busy || working(row.status)"
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
          :model-value="row.task.enabled"
          :disabled="busy"
          :aria-label="tr('Task aktiv', 'Task enabled')"
          @update:model-value="emit('toggle')"
        />
        {{
          row.task.enabled ? tr("Aktiv", "Enabled") : tr("Pausiert", "Disabled")
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
    <Alert v-if="row.retentionWarning"
      ><AlertDescription>{{ row.retentionWarning }}</AlertDescription></Alert
    >

    <PropertyList>
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
          <AlertDescription class="flex flex-wrap items-center justify-between gap-3">
            <span>{{ error }}</span>
            <Button variant="outline" size="sm" :disabled="loading" @click="load()">{{ tr('Erneut versuchen', 'Retry') }}</Button>
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
            :value="result.content.value"
            :label="tr('Ergebnis', 'Result')"
            :open-depth="2"
          />
        </template>
      </CardContent>
    </Card>
  </div>
</template>
