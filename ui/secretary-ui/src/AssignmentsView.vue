<script setup lang="ts">
import { computed, ref } from "vue";
import {
  Alert,
  AlertDescription,
  Button,
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
  Disclosure,
  JsonTree,
  Collapsible,
  CollapsibleContent,
  CollapsibleTrigger,
  Field,
  FieldDescription,
  FieldGroup,
  FieldLegend,
  FieldSet,
  Input,
  Label,
  OptionSelect,
  PageControls,
  PageHeader,
  RemoteState,
  SegmentedControl,
  Sheet,
  SheetContent,
  SheetDescription,
  SheetHeader,
  SheetTitle,
  Switch,
  Textarea,
  useRemote,
} from "@ivy/ui";
import { CalendarClock, ChevronRight, Plus, Zap } from "@lucide/vue";
import type {
  Assignment,
  ContactWindow,
  Document,
  EventTrigger,
  ObjectTrigger,
  RuleOverrides,
  ScheduleTrigger,
} from "./types";
import {
  listDocuments,
  listTopics,
  readDocument,
  saveAssignment,
  topicChoiceKey,
  tr,
} from "./runtime";
import type { TopicChoice } from "./runtime";
import { usePage } from "../../../packages/ui-client/src/runtime";
const props = defineProps<{ node: string; root: string }>();
const assignments = usePage(
  (signal, cursor) =>
    listDocuments("secretary/assignment", props.root, signal, 100, cursor),
  15000,
  () => "secretary-assignments:" + props.root,
  ["objects/secretary/assignment"],
);
const topics = useRemote((signal) => listTopics(signal), 30000, ["services"]);
const selected = ref<Document<Assignment> | null>(null),
  draft = ref<Assignment | null>(null),
  busy = ref(false),
  error = ref(""),
  message = ref("");
const schedule = computed(() =>
  draft.value?.trigger.kind === "schedule" ? draft.value.trigger : null,
);
const event = computed(() =>
  draft.value?.trigger.kind === "event" ? draft.value.trigger : null,
);
const objects = computed(() =>
  draft.value?.trigger.kind === "object-change" ? draft.value.trigger : null,
);
const objectIds = computed({
  get: () => objects.value?.objectIds.join("\n") ?? "",
  set: (value: string) => {
    if (objects.value)
      objects.value.objectIds = value
        .split(/[\n,]+/)
        .map((part) => part.trim())
        .filter(Boolean);
  },
});
const objectPaths = computed({
  get: () => objects.value?.paths.join("\n") ?? "",
  set: (value: string) => {
    if (objects.value)
      objects.value.paths = value
        .split(/\n+/)
        .map((part) => part.trim())
        .filter(Boolean);
  },
});
const setObservation = (enabled: boolean) => {
  if (!objects.value) return;
  objects.value.observation = enabled
    ? {
        completePath: "/data/observation/complete",
        observedAtPath: "/data/observation/observedAt",
        maximumAgeSeconds: 180,
      }
    : null;
};
const selectedTopic = computed<TopicChoice | null>(() => {
  if (!event.value) return null;
  return (
    topics.value.value?.find(
      (choice) =>
        (!event.value?.sourceServiceNodeId ||
          choice.provider?.node.serviceNodeId ===
            event.value.sourceServiceNodeId) &&
        choice.definition.topic === event.value?.topic &&
        choice.definition.version === event.value?.topicVersion,
    ) ?? null
  );
});
const selectedTopicKey = computed(() =>
  selectedTopic.value ? topicChoiceKey(selectedTopic.value) : "",
);
const unavailableTopic = computed(
  () =>
    !!event.value?.topic &&
    !topics.loading.value &&
    !topics.error.value &&
    !selectedTopic.value,
);
const weekdays = [
  tr("So", "Sun"),
  tr("Mo", "Mon"),
  tr("Di", "Tue"),
  tr("Mi", "Wed"),
  tr("Do", "Thu"),
  tr("Fr", "Fri"),
  tr("Sa", "Sat"),
];
const choose = (row: Document<Assignment>) => {
  selected.value = row;
  draft.value = structuredClone(row.value);
  draft.value.execution ??= { model: null, effort: null, reuse: "new" };
  advancedOpen.value = Object.keys(row.value.rules).length > 0;
  error.value = "";
  message.value = "";
};
const create = () => {
  const now = new Date().toISOString();
  selected.value = null;
  advancedOpen.value = false;
  draft.value = {
    schemaVersion: 1,
    assignmentId: crypto.randomUUID(),
    name: "",
    description: "",
    enabled: false,
    minimumNotificationAgeMinutes: 0,
    prompt: "",
    preflight: null,
    execution: { model: null, effort: null, reuse: "new" },
    rules: {},
    createdAt: now,
    updatedAt: now,
    trigger: {
      kind: "schedule",
      cadence: "daily",
      intervalMinutes: null,
      localTime: "09:00",
      weekdays: [],
      timeZone:
        Intl.DateTimeFormat().resolvedOptions().timeZone || "Europe/Berlin",
    },
  };
  error.value = "";
  message.value = "";
};
const showIdeas = ref(false);
const setNotificationAge = (value: string | number) => {
  if (!draft.value) return;
  if (value === "") delete draft.value.minimumNotificationAgeMinutes;
  else draft.value.minimumNotificationAgeMinutes = Number(value);
};
const startExample = (kind: "schedule" | "event") => {
  create();
  setKind(kind);
  draft.value!.name =
    kind === "schedule"
      ? tr("Regelmäßiger Überblick", "Regular overview")
      : tr("Auf Ereignisse reagieren", "Respond to events");
  draft.value!.prompt =
    kind === "schedule"
      ? tr(
          "Prüfe die Informationen zu [Thema] und fasse relevante Änderungen zusammen. Benachrichtige mich, wenn ich etwas tun oder wissen sollte.",
          "Review information about [topic] and summarize relevant changes. Notify me when there is something I should do or know.",
        )
      : tr(
          "Prüfe das auslösende Ereignis. Erledige [Aufgabe] und benachrichtige mich, wenn meine Aufmerksamkeit nötig ist.",
          "Inspect the triggering event. Carry out [task] and notify me when my attention is needed.",
        );
};
const setKind = (kind: string) => {
  if (!draft.value || draft.value.trigger.kind === kind) return;
  draft.value.trigger =
    kind === "schedule"
      ? {
          kind: "schedule",
          cadence: "daily",
          intervalMinutes: null,
          localTime: "09:00",
          weekdays: [],
          timeZone:
            Intl.DateTimeFormat().resolvedOptions().timeZone || "Europe/Berlin",
        }
      : kind === "object-change"
        ? ({
            kind: "object-change",
            objectIds: [],
            paths: [],
            delaySeconds: 0,
            observation: null,
          } satisfies ObjectTrigger)
        : {
            kind: "event",
            topic: "",
            topicVersion: "1.0.0",
            sourceServiceNodeId: null,
            eventKind: null,
          };
};
const chooseTopic = (key: string) => {
  if (!event.value) return;
  const choice = topics.value.value?.find(
    (candidate) => topicChoiceKey(candidate) === key,
  );
  if (!choice) return;
  const changed =
    event.value.topic !== choice.definition.topic ||
    event.value.topicVersion !== choice.definition.version;
  event.value.sourceServiceNodeId = choice.provider?.node.serviceNodeId ?? null;
  event.value.topic = choice.definition.topic;
  event.value.topicVersion = choice.definition.version;
  if (changed)
    event.value.eventKind =
      choice.definition.eventKinds.length === 1
        ? choice.definition.eventKinds[0]!.kind
        : null;
};
const setCadence = (value: string) => {
  if (!schedule.value) return;
  schedule.value.cadence = value as ScheduleTrigger["cadence"];
  schedule.value.intervalMinutes = value === "interval" ? 15 : null;
  schedule.value.localTime = value === "interval" ? null : "09:00";
  schedule.value.weekdays = value === "weekly" ? [1, 2, 3, 4, 5] : [];
};
const toggleDay = (day: number) => {
  if (!schedule.value) return;
  schedule.value.weekdays = schedule.value.weekdays.includes(day)
    ? schedule.value.weekdays.filter((value) => value !== day)
    : [...schedule.value.weekdays, day].sort();
};
const rule = (key: "main" | "voice") => draft.value?.rules[key];
const advancedOpen = ref(false);
const close = () => {
  draft.value = null;
  selected.value = null;
  error.value = "";
};
const kinds = computed(() => [
  { value: "schedule", label: tr("Zeitplan", "Schedule") },
  { value: "event", label: tr("Ereignis", "Event") },
  { value: "object-change", label: tr("Objektänderung", "Object change") },
]);
const triggerSummary = (value: Assignment) => {
  const trigger = value.trigger;
  if (trigger.kind === "event")
    return tr("Bei Ereignis", "On event") + " · " + trigger.topic;
  if (trigger.kind === "object-change")
    return (
      tr("Bei Objektänderung", "On object change") +
      " · " +
      trigger.objectIds.length
    );
  if (trigger.cadence === "interval")
    return tr(
      `Alle ${trigger.intervalMinutes} Minuten`,
      `Every ${trigger.intervalMinutes} minutes`,
    );
  if (trigger.cadence === "weekly")
    return (
      trigger.weekdays.map((day) => weekdays[day]).join(", ") +
      " · " +
      (trigger.localTime ?? "")
    );
  return tr("Täglich", "Daily") + " · " + (trigger.localTime ?? "");
};
const toggleRule = (key: "main" | "voice", enabled: boolean) => {
  if (!draft.value) return;
  if (enabled) draft.value.rules[key] = {};
  else delete draft.value.rules[key];
};
const has = (object: object | undefined, key: string) =>
  !!object && Object.prototype.hasOwnProperty.call(object, key);
const toggleField = (
  key: "main" | "voice",
  field: string,
  enabled: boolean,
) => {
  const value = rule(key) as Record<string, unknown> | undefined;
  if (!value) return;
  if (!enabled) delete value[field];
  else if (field === "enabled") value[field] = true;
  else if (field === "minimumUrgency")
    value[field] = key === "voice" ? "critical" : "normal";
  else if (field === "immediateOnly") value[field] = true;
  else if (field === "window")
    value[field] = {
      timeZone:
        Intl.DateTimeFormat().resolvedOptions().timeZone || "Europe/Berlin",
      start: "08:00",
      end: "22:00",
    };
};
const rules = () => draft.value!.rules as RuleOverrides;
const setWindowMode = (key: "main" | "voice", mode: string) => {
  const value = rule(key) as Record<string, unknown> | undefined;
  if (!value) return;
  value.window =
    mode === "none"
      ? null
      : {
          timeZone:
            Intl.DateTimeFormat().resolvedOptions().timeZone || "Europe/Berlin",
          start: "08:00",
          end: "22:00",
        };
};
async function save() {
  if (!draft.value || busy.value) return;
  busy.value = true;
  error.value = "";
  message.value = "";
  try {
    if (event.value) {
      event.value.sourceServiceNodeId =
        event.value.sourceServiceNodeId?.trim() || null;
      event.value.eventKind = event.value.eventKind?.trim() || null;
    }
    if (objects.value) {
      objects.value.objectIds = [...new Set(objects.value.objectIds)];
      objects.value.paths = [...new Set(objects.value.paths)];
    }
    const pin = await saveAssignment(
        props.node,
        selected.value?.pin ?? null,
        draft.value,
      ),
      loaded = await readDocument("secretary/assignment", pin, props.root);
    selected.value = loaded;
    draft.value = structuredClone(loaded.value);
    message.value = tr(
      `Revision ${pin.revision} gespeichert.`,
      `Saved revision ${pin.revision}.`,
    );
    await assignments.refresh();
  } catch (failure) {
    error.value = failure instanceof Error ? failure.message : String(failure);
  } finally {
    busy.value = false;
  }
}
async function toggle(row: Document<Assignment>) {
  if (busy.value) return;
  busy.value = true;
  error.value = "";
  try {
    await saveAssignment(props.node, row.pin, {
      ...structuredClone(row.value),
      enabled: !row.value.enabled,
    });
    await assignments.refresh();
  } catch (failure) {
    error.value = failure instanceof Error ? failure.message : String(failure);
  } finally {
    busy.value = false;
  }
}
</script>
<template>
  <PageHeader
    :title="tr('Aufträge', 'Assignments')"
    :loading="assignments.loading.value"
    :updated-at="assignments.updatedAt.value"
    ><Button variant="outline" size="sm" @click="showIdeas = !showIdeas">{{
      tr("Ideen", "Ideas")
    }}</Button
    ><Button
      size="sm"
      :aria-label="tr('Neuer Auftrag', 'New assignment')"
      @click="create"
      ><Plus aria-hidden="true" /><span class="hidden sm:inline">{{
        tr("Neuer Auftrag", "New assignment")
      }}</span></Button
    ></PageHeader
  >
  <section class="mx-auto w-full max-w-4xl space-y-4">
    <div
      v-if="
        assignments.value.value &&
        (showIdeas || assignments.value.value.items.length === 0)
      "
      class="grid gap-4 sm:grid-cols-2"
    >
      <Card>
        <CardHeader
          ><CalendarClock
            class="size-5 text-muted-foreground"
            aria-hidden="true"
          /><CardTitle>{{
            tr(
              "Regelmäßig prüfen und zusammenfassen",
              "Check and summarize regularly",
            )
          }}</CardTitle
          ><CardDescription>{{
            tr(
              "Zum Beispiel täglich einen Überblick zu einem Thema erhalten oder offene Aufgaben prüfen.",
              "Get a daily overview of a topic or review pending tasks.",
            )
          }}</CardDescription></CardHeader
        >
        <CardContent
          ><Button variant="outline" @click="startExample('schedule')">{{
            tr("Zeitplan entwerfen", "Draft a schedule")
          }}</Button></CardContent
        >
      </Card>
      <Card>
        <CardHeader
          ><Zap
            class="size-5 text-muted-foreground"
            aria-hidden="true"
          /><CardTitle>{{
            tr("Auf ein Ereignis reagieren", "Respond to an event")
          }}</CardTitle
          ><CardDescription>{{
            tr(
              "Einen Auftrag starten, wenn ein verbundener Dienst etwas meldet. Die verfügbaren Ereignisse kommen aus deiner Registry.",
              "Start an assignment when a connected service reports something. Available events come from your registry.",
            )
          }}</CardDescription></CardHeader
        >
        <CardContent
          ><Button variant="outline" @click="startExample('event')">{{
            tr("Ereignisauftrag entwerfen", "Draft an event assignment")
          }}</Button></CardContent
        >
      </Card>
      <p class="text-sm text-muted-foreground sm:col-span-2">
        {{
          tr(
            "Das öffnet einen Entwurf. Erst speichern und aktivieren startet den Auftrag.",
            "This opens a draft. Save and enable it to start the assignment.",
          )
        }}
      </p>
    </div>
    <RemoteState
      :loading="assignments.loading.value"
      :error="assignments.error.value"
      :has-data="!!assignments.value.value"
      :empty="assignments.value.value?.items.length === 0"
      :empty-title="tr('Keine Aufträge', 'No assignments')"
      :empty-detail="
        tr(
          'Erstelle einen Auftrag, der nach Zeitplan oder bei einem registrierten Ereignis läuft.',
          'Create an assignment to run work on a schedule or a registered event.',
        )
      "
      @retry="assignments.refresh"
    />
    <Alert v-if="error && !draft" variant="destructive"
      ><AlertDescription>{{ error }}</AlertDescription></Alert
    >
    <ul
      v-if="assignments.value.value?.items.length"
      class="divide-y overflow-hidden rounded-lg border bg-card"
    >
      <li
        v-for="row in assignments.value.value.items"
        :key="row.pin.objectId"
        class="flex items-center gap-3 px-4 py-3 hover:bg-muted/50"
        :class="{ 'bg-muted/50': selected?.pin.objectId === row.pin.objectId }"
      >
        <component
          :is="row.value.trigger.kind === 'schedule' ? CalendarClock : Zap"
          class="size-4 shrink-0 text-muted-foreground"
          aria-hidden="true"
        />
        <Button
          variant="ghost"
          class="h-auto min-w-0 flex-1 flex-col items-start gap-0 px-2 py-1 text-left font-normal"
          @click="choose(row)"
        >
          <span class="block truncate text-sm font-medium">{{
            row.value.name
          }}</span>
          <span class="block truncate text-xs text-muted-foreground">{{
            triggerSummary(row.value)
          }}</span>
        </Button>
        <Switch
          :model-value="row.value.enabled"
          :disabled="busy"
          :aria-label="row.value.name"
          @update:model-value="toggle(row)"
        />
      </li>
    </ul>
    <PageControls
      v-if="assignments.value.value?.nextCursor || assignments.page.value > 1"
      :page="assignments.page.value"
      :count="assignments.value.value?.items.length ?? 0"
      :has-next="!!assignments.value.value?.nextCursor"
      :loading="assignments.loading.value"
      @previous="assignments.previous"
      @next="assignments.next"
    />
  </section>

  <Sheet :open="!!draft" @update:open="!$event && close()">
    <SheetContent class="w-full gap-0 overflow-y-auto p-0 sm:max-w-xl">
      <SheetHeader class="border-b">
        <SheetTitle>{{
          selected
            ? tr("Auftrag bearbeiten", "Edit assignment")
            : tr("Neuer Auftrag", "New assignment")
        }}</SheetTitle>
        <SheetDescription>{{
          tr(
            "Der Prompt läuft im unten gewählten Task-Kontext.",
            "The prompt runs in the selected task context below.",
          )
        }}</SheetDescription>
      </SheetHeader>
      <form v-if="draft" class="flex flex-1 flex-col" @submit.prevent="save">
        <div class="flex-1 space-y-8 p-4">
          <FieldGroup>
            <Field
              ><Label for="assignment-name">{{ tr("Name", "Name") }}</Label
              ><Input
                id="assignment-name"
                v-model="draft.name"
                required
                maxlength="256"
            /></Field>
            <Field
              ><Label for="assignment-description">{{
                tr("Beschreibung", "Description")
              }}</Label
              ><Input
                id="assignment-description"
                v-model="draft.description"
                maxlength="2048"
                :placeholder="tr('Optional', 'Optional')"
            /></Field>
            <Field
              ><Label for="assignment-prompt">Prompt</Label
              ><Textarea
                id="assignment-prompt"
                v-model="draft.prompt"
                required
                rows="7"
                maxlength="131072"
            /></Field>
          </FieldGroup>

          <FieldSet v-if="draft.execution">
            <FieldLegend>{{ tr("Ausführung", "Execution") }}</FieldLegend>
            <div class="grid gap-4 sm:grid-cols-3">
              <Field>
                <Label for="assignment-model">{{
                  tr("Modell (leer = global)", "Model (blank = global)")
                }}</Label>
                <Input
                  id="assignment-model"
                  :model-value="draft.execution.model ?? ''"
                  @update:model-value="
                    draft!.execution!.model = String($event).trim() || null
                  "
                />
              </Field>
              <Field>
                <Label for="assignment-effort">Reasoning</Label>
                <OptionSelect
                  id="assignment-effort"
                  :model-value="draft.execution.effort ?? ''"
                  class="w-full"
                  @update:model-value="
                    draft!.execution!.effort = (String($event) ||
                      null) as NonNullable<Assignment['execution']>['effort']
                  "
                >
                  <option value="">{{ tr("Global", "Global") }}</option>
                  <option value="low">Low</option>
                  <option value="medium">Medium</option>
                  <option value="high">High</option>
                  <option value="xhigh">XHigh</option>
                </OptionSelect>
              </Field>
              <Field>
                <Label for="assignment-reuse">{{
                  tr("Task-Nutzung", "Task reuse")
                }}</Label>
                <OptionSelect
                  id="assignment-reuse"
                  v-model="draft.execution.reuse"
                  class="w-full"
                >
                  <option value="main">
                    {{
                      tr("Gemeinsamer Secretary-Task", "Shared Secretary task")
                    }}
                  </option>
                  <option value="assignment">
                    {{
                      tr(
                        "Eigener wiederverwendeter Task",
                        "Dedicated reused task",
                      )
                    }}
                  </option>
                  <option value="new">
                    {{ tr("Immer neuer Task", "New task every time") }}
                  </option>
                </OptionSelect>
              </Field>
            </div>
          </FieldSet>

          <FieldSet>
            <FieldLegend>Trigger</FieldLegend>
            <SegmentedControl
              :model-value="draft.trigger.kind"
              :label="tr('Trigger-Typ', 'Trigger type')"
              :options="kinds"
              @update:model-value="setKind($event)"
            />
            <div v-if="schedule" class="grid gap-4 sm:grid-cols-2">
              <Field
                ><Label for="schedule-cadence">{{
                  tr("Rhythmus", "Cadence")
                }}</Label
                ><OptionSelect
                  id="schedule-cadence"
                  :model-value="schedule.cadence"
                  class="w-full"
                  @update:model-value="setCadence(String($event))"
                  ><option value="interval">
                    {{ tr("Intervall", "Interval") }}
                  </option>
                  <option value="daily">{{ tr("Täglich", "Daily") }}</option>
                  <option value="weekly">
                    {{ tr("Wöchentlich", "Weekly") }}
                  </option></OptionSelect
                ></Field
              >
              <Field v-if="schedule.cadence === 'interval'"
                ><Label for="schedule-interval">{{
                  tr("Alle Minuten", "Every (minutes)")
                }}</Label
                ><Input
                  id="schedule-interval"
                  :model-value="schedule.intervalMinutes ?? 15"
                  type="number"
                  min="1"
                  max="10080"
                  required
                  @update:model-value="
                    schedule!.intervalMinutes = Number($event)
                  "
              /></Field>
              <Field v-else
                ><Label for="schedule-time">{{
                  tr("Uhrzeit", "Local time")
                }}</Label
                ><Input
                  id="schedule-time"
                  :model-value="schedule.localTime ?? '09:00'"
                  type="time"
                  required
                  @update:model-value="schedule!.localTime = String($event)"
              /></Field>
              <Field v-if="schedule.cadence === 'weekly'" class="sm:col-span-2"
                ><Label id="schedule-weekdays">{{
                  tr("Wochentage", "Weekdays")
                }}</Label>
                <div
                  class="flex flex-wrap gap-1.5"
                  role="group"
                  aria-labelledby="schedule-weekdays"
                >
                  <Button
                    v-for="(label, day) in weekdays"
                    :key="day"
                    type="button"
                    size="sm"
                    class="w-12"
                    :variant="
                      schedule.weekdays.includes(day) ? 'default' : 'outline'
                    "
                    :aria-pressed="schedule.weekdays.includes(day)"
                    @click="toggleDay(day)"
                    >{{ label }}</Button
                  >
                </div></Field
              >
              <Field class="sm:col-span-2"
                ><Label for="schedule-zone">{{
                  tr("Zeitzone", "Time zone")
                }}</Label
                ><Input id="schedule-zone" v-model="schedule.timeZone" required
              /></Field>
            </div>
            <div v-if="objects" class="space-y-4">
              <Field>
                <Label for="trigger-objects">{{
                  tr("Hive-Objekte", "Hive objects")
                }}</Label>
                <Textarea
                  id="trigger-objects"
                  v-model="objectIds"
                  rows="3"
                  required
                />
                <FieldDescription>{{
                  tr(
                    "Eine Objekt-ID pro Zeile. Die erste Beobachtung legt den Ausgangszustand fest.",
                    "One object ID per line. The first observation establishes the baseline.",
                  )
                }}</FieldDescription>
              </Field>
              <Field>
                <Label for="trigger-paths">{{
                  tr("Beobachtete Inhalte", "Observed content")
                }}</Label>
                <Textarea
                  id="trigger-paths"
                  v-model="objectPaths"
                  rows="3"
                  placeholder="/data/activity"
                />
                <FieldDescription>{{
                  tr(
                    "Ein JSON Pointer pro Zeile. Leer beobachtet das gesamte Objekt. Wähle Inhaltsfelder, um laufende Zeitstempel auszuschließen.",
                    "One JSON Pointer per line. Leave blank to observe the whole object. Select content fields to exclude changing timestamps.",
                  )
                }}</FieldDescription>
              </Field>
              <Field>
                <Label for="trigger-delay">{{
                  tr("Änderungen bündeln (Sekunden)", "Group changes (seconds)")
                }}</Label>
                <Input
                  id="trigger-delay"
                  :model-value="objects.delaySeconds"
                  type="number"
                  min="0"
                  max="3600"
                  @update:model-value="objects!.delaySeconds = Number($event)"
                />
              </Field>
              <Field orientation="horizontal">
                <Label for="trigger-observation">{{
                  tr(
                    "Vollständige, aktuelle Beobachtung abwarten",
                    "Wait for a complete, recent observation",
                  )
                }}</Label>
                <Switch
                  id="trigger-observation"
                  :model-value="!!objects.observation"
                  @update:model-value="setObservation($event)"
                />
              </Field>
              <div v-if="objects.observation" class="grid gap-4 sm:grid-cols-2">
                <Field>
                  <Label for="trigger-complete">{{
                    tr("Pfad zur Vollständigkeit", "Completeness path")
                  }}</Label>
                  <Input
                    id="trigger-complete"
                    v-model="objects.observation.completePath"
                    required
                  />
                </Field>
                <Field>
                  <Label for="trigger-observed">{{
                    tr(
                      "Pfad zum Beobachtungszeitpunkt",
                      "Observation time path",
                    )
                  }}</Label>
                  <Input
                    id="trigger-observed"
                    v-model="objects.observation.observedAtPath"
                    required
                  />
                </Field>
                <Field>
                  <Label for="trigger-age">{{
                    tr(
                      "Maximales Beobachtungsalter (Sekunden)",
                      "Maximum observation age (seconds)",
                    )
                  }}</Label>
                  <Input
                    id="trigger-age"
                    :model-value="objects.observation.maximumAgeSeconds"
                    type="number"
                    min="1"
                    max="86400"
                    @update:model-value="
                      objects!.observation!.maximumAgeSeconds = Number($event)
                    "
                  />
                </Field>
              </div>
              <FieldDescription>{{
                tr(
                  "Änderungen lösen einen normalen Secretary-Auftrag aus. Während der Bündelung zurückgenommene Änderungen lösen keinen Auftrag aus.",
                  "Changes start a regular Secretary assignment. Changes reverted during grouping do not start an assignment.",
                )
              }}</FieldDescription>
            </div>
            <div v-if="event" class="space-y-4">
              <RemoteState
                :loading="topics.loading.value"
                :error="topics.error.value"
                :has-data="!!topics.value.value"
                :empty="topics.value.value?.length === 0"
                :empty-title="
                  tr('Keine Ereignisse veröffentlicht', 'No events published')
                "
                @retry="topics.refresh"
              />
              <Field v-if="topics.value.value?.length"
                ><Label for="event-topic">{{
                  tr("Veröffentlichtes Ereignis", "Published event")
                }}</Label
                ><OptionSelect
                  id="event-topic"
                  :model-value="selectedTopicKey"
                  required
                  class="w-full"
                  @update:model-value="chooseTopic(String($event))"
                  ><option value="" disabled>
                    {{ tr("Ereignis auswählen…", "Select an event…") }}
                  </option>
                  <option
                    v-for="choice in topics.value.value"
                    :key="topicChoiceKey(choice)"
                    :value="topicChoiceKey(choice)"
                  >
                    {{ choice.definition.title }} ·
                    {{ choice.definition.version }} ·
                    {{ choice.provider?.node.serviceName ?? "Hive" }} ·
                    {{ choice.provider?.node.hostId ?? ""
                    }}{{
                      choice.provider?.eligible !== false ? "" : " · offline"
                    }}
                  </option></OptionSelect
                ><FieldDescription v-if="selectedTopic">{{
                  selectedTopic.definition.description
                }}</FieldDescription></Field
              >
              <Field v-if="selectedTopic?.definition.eventKinds.length"
                ><Label for="event-kind">{{
                  tr("Ereignisart", "Event kind")
                }}</Label
                ><OptionSelect
                  id="event-kind"
                  :model-value="event.eventKind ?? ''"
                  class="w-full"
                  @update:model-value="
                    event!.eventKind = String($event).trim() || null
                  "
                  ><option value="">
                    {{ tr("Alle Ereignisarten", "All event kinds") }}
                  </option>
                  <option
                    v-for="kind in selectedTopic.definition.eventKinds"
                    :key="kind.kind"
                    :value="kind.kind"
                  >
                    {{ kind.title }} — {{ kind.description }}
                  </option></OptionSelect
                ></Field
              >
              <Disclosure
                v-if="selectedTopic"
                :title="tr('Ereignisdaten', 'Event data')"
                ><JsonTree :value="selectedTopic.definition.payloadSchema"
              /></Disclosure>
              <p v-if="selectedTopic" class="text-sm text-muted-foreground">
                {{
                  tr(
                    "Nachrichtenereignisse mit messageChannel werden je Unterhaltung fünf Minuten gebündelt. Secretary prüft danach den Lesestatus beim Quelldienst; gelesene Unterhaltungen lösen keinen Auftrag aus.",
                    "Message events with messageChannel are grouped per conversation for five minutes. Secretary then checks unread state with the source; read conversations do not start an assignment.",
                  )
                }}
              </p>
              <Alert v-if="unavailableTopic"
                ><AlertDescription>{{
                  tr(
                    `Der gespeicherte Trigger ${event.topic}@${event.topicVersion} von ${event.sourceServiceNodeId ?? "beliebiger Quelle"} wird aktuell von keinem Service veröffentlicht. Er bleibt unverändert gespeichert, bis du ein anderes Ereignis auswählst.`,
                    `The saved trigger ${event.topic}@${event.topicVersion} from ${event.sourceServiceNodeId ?? "any source"} is not currently published by a service. It remains unchanged until you select another event.`,
                  )
                }}</AlertDescription></Alert
              >
            </div>
          </FieldSet>

          <Field>
            <Label for="notification-age">{{
              tr(
                "Mindestalter vor Main-Benachrichtigung (Minuten)",
                "Minimum age before Main notification (minutes)",
              )
            }}</Label>
            <Input
              id="notification-age"
              type="number"
              min="0"
              max="1440"
              :model-value="draft.minimumNotificationAgeMinutes ?? ''"
              @update:model-value="setNotificationAge"
            />
            <FieldDescription>{{
              tr(
                "Leer übernimmt den Dienststandard. 0 bedeutet sofort. Dringende Anrufe warten nicht auf diese Frist.",
                "Leave blank to use the service default. 0 means immediately. Urgent calls do not wait for this delay.",
              )
            }}</FieldDescription>
          </Field>
          <Collapsible v-model:open="advancedOpen">
            <CollapsibleTrigger as-child
              ><Button
                type="button"
                variant="ghost"
                class="-ml-3 text-muted-foreground"
                ><ChevronRight
                  class="transition-transform"
                  :class="{ 'rotate-90': advancedOpen }"
                  aria-hidden="true"
                />{{ tr("Regelabweichungen", "Rule overrides") }}</Button
              ></CollapsibleTrigger
            >
            <CollapsibleContent class="space-y-6 pt-3">
              <p class="text-sm text-muted-foreground">
                {{
                  tr(
                    "Nicht gesetzte Felder lesen immer den aktuellen globalen Wert.",
                    "Unset fields always use the current global setting.",
                  )
                }}
              </p>
              <FieldSet
                v-for="key in ['main', 'voice'] as const"
                :key="key"
                class="rounded-lg border p-4"
              >
                <Field orientation="horizontal"
                  ><Label :for="'override-' + key" class="flex-1">{{
                    key === "voice"
                      ? tr("Anruf überschreiben", "Override call")
                      : tr("Main überschreiben", "Override Main")
                  }}</Label
                  ><Switch
                    :id="'override-' + key"
                    :model-value="!!rule(key)"
                    @update:model-value="toggleRule(key, $event === true)"
                /></Field>
                <template v-if="rule(key)">
                  <Field orientation="horizontal"
                    ><Label
                      :for="'override-enabled-' + key"
                      class="flex-1 font-normal"
                      >{{ tr("Aktiv festlegen", "Set enabled") }}</Label
                    ><Switch
                      :id="'override-enabled-' + key"
                      :model-value="has(rule(key), 'enabled')"
                      @update:model-value="
                        toggleField(key, 'enabled', $event === true)
                      "
                  /></Field>
                  <Field
                    v-if="has(rule(key), 'enabled')"
                    orientation="horizontal"
                    class="pl-4"
                    ><Label
                      :for="'rule-enabled-' + key"
                      class="flex-1 font-normal"
                      >{{ tr("Aktiv", "Enabled") }}</Label
                    ><Switch
                      :id="'rule-enabled-' + key"
                      v-model="(rules()[key] as any).enabled"
                  /></Field>
                  <Field v-if="key !== 'voice'" orientation="horizontal"
                    ><Label
                      :for="'override-urgency-' + key"
                      class="flex-1 font-normal"
                      >{{
                        tr("Dringlichkeit festlegen", "Set minimum urgency")
                      }}</Label
                    ><Switch
                      :id="'override-urgency-' + key"
                      :model-value="has(rule(key), 'minimumUrgency')"
                      @update:model-value="
                        toggleField(key, 'minimumUrgency', $event === true)
                      "
                  /></Field>
                  <OptionSelect
                    v-if="has(rule(key), 'minimumUrgency') && key !== 'voice'"
                    v-model="(rules()[key] as any).minimumUrgency"
                    class="w-full"
                    :aria-label="tr('Mindestdringlichkeit', 'Minimum urgency')"
                    ><option value="normal">Normal</option>
                    <option value="high">High</option>
                    <option value="critical">Critical</option></OptionSelect
                  >
                  <Field orientation="horizontal"
                    ><Label
                      :for="'override-window-' + key"
                      class="flex-1 font-normal"
                      >{{
                        tr("Kontaktfenster festlegen", "Set contact window")
                      }}</Label
                    ><Switch
                      :id="'override-window-' + key"
                      :model-value="has(rule(key), 'window')"
                      @update:model-value="
                        toggleField(key, 'window', $event === true)
                      "
                  /></Field>
                  <template v-if="has(rule(key), 'window')">
                    <OptionSelect
                      :model-value="
                        (rules()[key] as any).window === null
                          ? 'none'
                          : 'custom'
                      "
                      class="w-full"
                      :aria-label="tr('Kontaktfenster', 'Contact window')"
                      @update:model-value="setWindowMode(key, String($event))"
                      ><option value="custom">
                        {{
                          tr("Eigenes Kontaktfenster", "Custom contact window")
                        }}
                      </option>
                      <option value="none">
                        {{ tr("Keine Zeitbegrenzung", "No time restriction") }}
                      </option></OptionSelect
                    >
                    <div
                      v-if="(rules()[key] as any).window !== null"
                      class="grid grid-cols-2 gap-3"
                    >
                      <Field class="col-span-2"
                        ><Label :for="'window-zone-' + key">{{
                          tr("Zeitzone", "Time zone")
                        }}</Label
                        ><Input
                          :id="'window-zone-' + key"
                          v-model="
                            ((rules()[key] as any).window as ContactWindow)
                              .timeZone
                          "
                      /></Field>
                      <Field
                        ><Label :for="'window-start-' + key">{{
                          tr("Von", "From")
                        }}</Label
                        ><Input
                          :id="'window-start-' + key"
                          v-model="
                            ((rules()[key] as any).window as ContactWindow)
                              .start
                          "
                          type="time"
                      /></Field>
                      <Field
                        ><Label :for="'window-end-' + key">{{
                          tr("Bis", "To")
                        }}</Label
                        ><Input
                          :id="'window-end-' + key"
                          v-model="
                            ((rules()[key] as any).window as ContactWindow).end
                          "
                          type="time"
                      /></Field>
                    </div>
                  </template>
                  <template v-if="key === 'voice'">
                    <Field orientation="horizontal"
                      ><Label
                        for="override-immediate"
                        class="flex-1 font-normal"
                        >{{
                          tr("„Nur sofort“ festlegen", "Set immediate only")
                        }}</Label
                      ><Switch
                        id="override-immediate"
                        :model-value="has(rule(key), 'immediateOnly')"
                        @update:model-value="
                          toggleField(key, 'immediateOnly', $event === true)
                        "
                    /></Field>
                    <Field
                      v-if="has(rule(key), 'immediateOnly')"
                      orientation="horizontal"
                      class="pl-4"
                      ><Label for="rule-immediate" class="flex-1 font-normal">{{
                        tr("Nur sofort", "Immediate only")
                      }}</Label
                      ><Switch
                        id="rule-immediate"
                        v-model="(rules().voice as any).immediateOnly"
                    /></Field>
                  </template>
                </template>
              </FieldSet>
              <FieldSet class="rounded-lg border p-4">
                <Field orientation="horizontal"
                  ><Label for="override-research" class="flex-1">{{
                    tr("Recherchezeit überschreiben", "Override research time")
                  }}</Label
                  ><Switch
                    id="override-research"
                    :model-value="has(draft.rules, 'researchMaxMinutes')"
                    @update:model-value="
                      $event === true
                        ? (draft!.rules.researchMaxMinutes = 5)
                        : delete draft!.rules.researchMaxMinutes
                    "
                /></Field>
                <Field v-if="has(draft.rules, 'researchMaxMinutes')"
                  ><Label for="research-minutes">{{
                    tr("Minuten", "Minutes")
                  }}</Label
                  ><Input
                    id="research-minutes"
                    :model-value="draft.rules.researchMaxMinutes ?? 5"
                    type="number"
                    min="0"
                    max="60"
                    @update:model-value="
                      draft!.rules.researchMaxMinutes = Number($event)
                    "
                /></Field>
              </FieldSet>
            </CollapsibleContent>
          </Collapsible>
          <Alert v-if="error" variant="destructive"
            ><AlertDescription>{{ error }}</AlertDescription></Alert
          >
        </div>
        <div
          class="sticky bottom-0 flex flex-wrap items-center gap-3 border-t bg-background p-4"
        >
          <Label class="mr-auto font-normal"
            ><Switch v-model="draft.enabled" />{{
              tr("Aktiv", "Enabled")
            }}</Label
          >
          <p v-if="message" role="status" class="text-sm text-muted-foreground">
            {{ message }}
          </p>
          <Button type="button" variant="ghost" @click="close">{{
            tr("Schließen", "Close")
          }}</Button>
          <Button type="submit" :disabled="busy">{{
            busy
              ? tr("Speichert…", "Saving…")
              : tr("Auftrag speichern", "Save assignment")
          }}</Button>
        </div>
      </form>
    </SheetContent>
  </Sheet>
</template>
