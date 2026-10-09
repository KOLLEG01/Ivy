<script setup lang="ts">
import { ref } from "vue";
import {
  Alert,
  AlertDescription,
  Button,
  Field,
  FieldDescription,
  Input,
  Label,
  OptionSelect,
  PageHeader,
  SettingsSection,
  Switch,
  useRemote,
} from "@ivy/ui";
import type { Operation } from "../../../packages/sdk/src/client.js";
import type {
  ContactRule,
  ContactWindow,
  Pin,
  SecretaryConfiguration,
  VoiceRule,
} from "./types";
import { client, readDocument, saveConfiguration, tr } from "./runtime";
const props = defineProps<{
  node: string;
  hostId: string;
  root: string;
  configuration: Pin | null;
}>();
const draft = ref<SecretaryConfiguration | null>(null),
  baseline = ref(""),
  pin = ref<Pin | null>(props.configuration),
  busy = ref(false),
  error = ref(""),
  message = ref("");
const managers = useRemote(
  async (signal) => {
    const result: Operation.ServiceNodesListResult = await client.request(
      "serviceNodes.list",
      { serviceName: "agent-manager", limit: 200 },
      { signal },
    );
    return result.items;
  },
  30000,
  ["services/agent-manager"],
);
const current = useRemote(
  async (signal) => {
    if (!pin.value)
      throw new Error(
        tr(
          "Secretary hat noch keine Konfiguration angelegt.",
          "Secretary has not created its configuration yet.",
        ),
      );
    const value = await readDocument(
      "secretary/configuration",
      pin.value.objectId,
      props.root,
      signal,
    );
    if (
      !busy.value &&
      value.pin.revision >= pin.value.revision &&
      (!draft.value || JSON.stringify(draft.value) === baseline.value)
    ) {
      draft.value = structuredClone(value.value);
      baseline.value = JSON.stringify(value.value);
      pin.value = value.pin;
    }
    return value;
  },
  0,
  ["objects/secretary/configuration"],
);
const ensureExecution = () => {
  if (!draft.value) return;
  draft.value.execution ??= {
    serviceNodeId:
      managers.value.value?.find(
        (value) =>
          value.hostId === props.hostId && value.ready && value.connected,
      )?.serviceNodeId ??
      managers.value.value?.find((value) => value.ready && value.connected)
        ?.serviceNodeId ??
      "",
    threadCwd: "",
    model: null,
    effort: "medium",
    permissions: ":read-only",
  };
};
const setWindow = (rule: ContactRule | VoiceRule, enabled: boolean) => {
  rule.window = enabled
    ? {
        timeZone:
          Intl.DateTimeFormat().resolvedOptions().timeZone || "Europe/Berlin",
        start: "08:00",
        end: "22:00",
      }
    : null;
};
const windowValue = (rule: ContactRule | VoiceRule) =>
  rule.window as ContactWindow;
const contactRules: {
  key: "main" | "voice";
  title: string;
  description: string;
}[] = [
  {
    key: "main",
    title: "Main",
    description: tr(
      "Normale asynchrone Hinweise in der Hauptunterhaltung des gewählten Ziels.",
      "Normal asynchronous notices in the selected target's primary conversation.",
    ),
  },
  {
    key: "voice",
    title: tr("Anruf", "Call"),
    description: tr(
      "Unterbrechender Anruf nur für außergewöhnlich dringende Situationen.",
      "An interrupting call reserved for exceptionally urgent situations.",
    ),
  },
];
async function save() {
  if (!draft.value || !pin.value || busy.value) return;
  const submitted = JSON.stringify(draft.value);
  busy.value = true;
  error.value = "";
  message.value = "";
  try {
    const next = await saveConfiguration(props.node, pin.value, JSON.parse(submitted));
    pin.value = next;
    const loaded = await readDocument(
      "secretary/configuration",
      next,
      props.root,
    );
    if (JSON.stringify(draft.value) === submitted)
      draft.value = structuredClone(loaded.value);
    baseline.value = JSON.stringify(loaded.value);
    message.value = tr(
      `Revision ${next.revision} gespeichert.`,
      `Saved revision ${next.revision}.`,
    );
  } catch (failure) {
    error.value = failure instanceof Error ? failure.message : String(failure);
  } finally {
    busy.value = false;
  }
}
</script>
<template>
  <PageHeader
    :title="tr('Einstellungen', 'Settings')"
    :loading="current.loading.value"
  />
  <section class="mx-auto w-full max-w-4xl">
    <Alert v-if="current.error.value || error" variant="destructive" class="mb-6"
      ><AlertDescription>{{
        current.error.value || error
      }}</AlertDescription></Alert
    >
    <template v-if="draft">
      <SettingsSection
        v-for="entry in contactRules"
        :key="entry.key"
        :title="entry.title"
        :description="entry.description"
      >
        <div class="space-y-4">
          <Field orientation="horizontal"
            ><Label :for="'rule-' + entry.key" class="flex-1">{{
              tr("Kontakt erlauben", "Allow contact")
            }}</Label
            ><Switch
              :id="'rule-' + entry.key"
              v-model="draft.rules[entry.key].enabled"
              :aria-label="entry.title"
          /></Field>
          <Field v-if="entry.key !== 'voice'"
            ><Label :for="'urgency-' + entry.key">{{
              tr("Mindestdringlichkeit", "Minimum urgency")
            }}</Label
            ><OptionSelect
              :id="'urgency-' + entry.key"
              v-model="draft.rules[entry.key].minimumUrgency"
              class="w-full"
              ><option value="normal">Normal</option>
              <option value="high">High</option>
              <option value="critical">Critical</option></OptionSelect
            ></Field
          >
          <Field orientation="horizontal"
            ><Label :for="'window-' + entry.key" class="flex-1 font-normal">{{
              tr("Kontaktfenster begrenzen", "Limit contact window")
            }}</Label
            ><Switch
              :id="'window-' + entry.key"
              :model-value="!!draft.rules[entry.key].window"
              @update:model-value="
                setWindow(draft!.rules[entry.key], $event === true)
              "
          /></Field>
          <div
            v-if="draft.rules[entry.key].window"
            class="grid grid-cols-2 gap-3"
          >
            <Field class="col-span-2"
              ><Label :for="'zone-' + entry.key">{{
                tr("Zeitzone", "Time zone")
              }}</Label
              ><Input
                :id="'zone-' + entry.key"
                v-model="windowValue(draft.rules[entry.key]).timeZone"
            /></Field>
            <Field
              ><Label :for="'from-' + entry.key">{{ tr("Von", "From") }}</Label
              ><Input
                :id="'from-' + entry.key"
                v-model="windowValue(draft.rules[entry.key]).start"
                type="time"
            /></Field>
            <Field
              ><Label :for="'to-' + entry.key">{{ tr("Bis", "To") }}</Label
              ><Input
                :id="'to-' + entry.key"
                v-model="windowValue(draft.rules[entry.key]).end"
                type="time"
            /></Field>
          </div>
          <Field v-if="entry.key === 'voice'" orientation="horizontal"
            ><Label for="immediate-only" class="flex-1 font-normal">{{
              tr("Nur sofortige Eskalationen", "Immediate escalations only")
            }}</Label
            ><Switch id="immediate-only" v-model="draft.rules.voice.immediateOnly"
          /></Field>
        </div>
      </SettingsSection>
      <SettingsSection
        :title="tr('Ausführungsziel', 'Execution target')"
        :description="
          tr(
            'Jede Ausführung startet auf diesem AgentManager eine neue persistente Codex-Task.',
            'Every run starts a new persistent Codex task on this AgentManager.',
          )
        "
      >
        <Button
          v-if="!draft.execution"
          variant="outline"
          @click="ensureExecution"
          >{{ tr("Ziel konfigurieren", "Configure target") }}</Button
        >
        <div v-else class="grid gap-4 sm:grid-cols-2">
          <Field
            ><Label for="execution-manager">AgentManager</Label
            ><OptionSelect
              id="execution-manager"
              v-model="draft.execution.serviceNodeId"
              class="w-full"
              ><option value="">—</option>
              <option
                v-for="manager in managers.value.value ?? []"
                :key="manager.serviceNodeId"
                :value="manager.serviceNodeId"
              >
                {{ manager.serviceNodeId
                }}{{ manager.ready && manager.connected ? "" : " · offline" }}
              </option></OptionSelect
            ></Field
          >
          <Field
            ><Label for="execution-cwd">{{
              tr("Arbeitsverzeichnis", "Working directory")
            }}</Label
            ><Input
              id="execution-cwd"
              v-model="draft.execution.threadCwd"
              :placeholder="tr('Absoluter Pfad', 'Absolute path')"
          /></Field>
          <Field
            ><Label for="execution-model">{{ tr("Modell", "Model") }}</Label
            ><Input
              id="execution-model"
              :model-value="draft.execution.model ?? ''"
              :placeholder="tr('Standard', 'Default')"
              @update:model-value="
                draft!.execution!.model = String($event).trim() || null
              "
          /></Field>
          <Field
            ><Label for="execution-effort">Reasoning</Label
            ><OptionSelect
              id="execution-effort"
              v-model="draft.execution.effort"
              class="w-full"
              ><option value="low">Low</option>
              <option value="medium">Medium</option>
              <option value="high">High</option>
              <option value="xhigh">XHigh</option></OptionSelect
            ></Field
          >
          <Field
            ><Label for="execution-permissions">{{
              tr("Berechtigungen", "Permissions")
            }}</Label
            ><OptionSelect
              id="execution-permissions"
              v-model="draft.execution.permissions"
              class="w-full"
              ><option value=":read-only">
                {{ tr("Nur lesen", "Read only") }}
              </option>
              <option value=":workspace">Workspace</option>
              <option value=":danger-full-access">
                {{ tr("Voller Zugriff", "Full access") }}
              </option></OptionSelect
            ></Field
          >
          <Field
            ><Label for="research-max">{{
              tr("Max. Recherchezeit", "Max research time")
            }}</Label
            ><Input
              id="research-max"
              v-model.number="draft.rules.researchMaxMinutes"
              type="number"
              min="0"
              max="60"
            /><FieldDescription>{{
              tr("Minuten", "Minutes")
            }}</FieldDescription></Field
          >
        </div>
      </SettingsSection>
      <div
        class="sticky bottom-0 flex flex-wrap items-center justify-end gap-3 border-t bg-background py-3"
      >
        <p v-if="message" role="status" class="mr-auto text-sm text-muted-foreground">
          {{ message }}
        </p>
        <Button :disabled="busy || !pin" @click="save">{{
          busy ? tr("Speichert…", "Saving…") : tr("Speichern", "Save")
        }}</Button>
      </div>
    </template>
  </section>
</template>
