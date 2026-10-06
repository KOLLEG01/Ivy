<script setup lang="ts">
import { computed, reactive, ref, watch } from "vue";
import {
  Button,
  Disclosure,
  Field,
  FieldDescription,
  FieldError,
  FieldGroup,
  FieldLabel,
  FieldLegend,
  FieldSet,
  Input,
  Switch,
  Textarea,
} from "@ivy/ui";
import { newTask, slugOf, tr } from "./collector";
import type { Task, TasksView } from "./collector";

// A structured form for every task setting; only config and dependencies remain JSON.
const props = defineProps<{
  task: Task | null;
  busy: boolean;
  limits: TasksView["limits"];
}>();
const emit = defineEmits<{ save: [task: Task]; cancel: [] }>();
const creating = !props.task;
const initial = props.task ?? newTask();
const form = reactive({
  ...initial,
  secretList: initial.secretNames.join(", "),
  inputSecret: initial.inputSecretName ?? "",
  allowUnauthenticated: initial.allowUnauthenticatedInput === true,
  retentionMb: Math.round(initial.retention.maximumBytes / 1048576),
});
const config = ref(JSON.stringify(initial.config, null, 2)),
  dependencies = ref(JSON.stringify(initial.dependencies, null, 2));
const idTouched = ref(!creating);
watch(
  () => form.name,
  (name) => {
    if (!idTouched.value) form.id = slugOf(name);
  },
);
const parse = (text: string) => {
  try {
    const value: unknown = JSON.parse(text || "{}");
    return value && typeof value === "object" && !Array.isArray(value)
      ? { value: value as Record<string, never>, error: "" }
      : {
          value: null,
          error: tr("Ein JSON-Objekt angeben.", "Enter a JSON object."),
        };
  } catch (cause) {
    return {
      value: null,
      error: cause instanceof Error ? cause.message : String(cause),
    };
  }
};
const configState = computed(() => parse(config.value)),
  dependencyState = computed(() => parse(dependencies.value));
const idError = computed(() =>
  /^[a-z0-9][a-z0-9_-]{0,63}$/.test(form.id)
    ? ""
    : tr(
        "Kleinbuchstaben, Ziffern, - und _; beginnt mit Buchstabe oder Ziffer.",
        "Lowercase letters, digits, - and _; starts with a letter or digit.",
      ),
);
const valid = computed(
  () =>
    !!form.name.trim() &&
    !idError.value &&
    !configState.value.error &&
    !dependencyState.value.error &&
    !!form.script.trim(),
);
const submit = () => {
  if (!valid.value) return;
  const secretNames = form.secretList
    .split(/[\s,]+/)
    .map((name) => name.trim())
    .filter(Boolean);
  emit("save", {
    id: form.id,
    name: form.name.trim(),
    enabled: form.enabled,
    intervalSeconds: Number(form.intervalSeconds),
    timeoutSeconds: Number(form.timeoutSeconds),
    memoryMb: Number(form.memoryMb),
    script: form.script,
    dependencies: dependencyState.value.value!,
    config: configState.value.value!,
    secretNames,
    ...(form.inputSecret ? { inputSecretName: form.inputSecret } : {}),
    ...(form.allowUnauthenticated && !form.inputSecret
      ? { allowUnauthenticatedInput: true }
      : {}),
    retention: {
      maximumCount: Number(form.retention.maximumCount),
      maximumAgeDays: Number(form.retention.maximumAgeDays),
      maximumBytes: Math.max(1, Number(form.retentionMb)) * 1048576,
    },
  });
};
</script>

<template>
  <form class="mx-auto w-full max-w-4xl space-y-8" @submit.prevent="submit">
    <FieldSet>
      <FieldLegend>{{ tr("Task", "Task") }}</FieldLegend>
      <FieldGroup class="grid gap-4 md:grid-cols-2">
        <Field>
          <FieldLabel for="task-name">{{ tr("Name", "Name") }}</FieldLabel>
          <Input
            id="task-name"
            v-model="form.name"
            required
            maxlength="200"
            :disabled="busy"
          />
        </Field>
        <Field :data-invalid="!!idError || undefined">
          <FieldLabel for="task-id">{{ tr("Task-ID", "Task ID") }}</FieldLabel>
          <Input
            id="task-id"
            v-model="form.id"
            :disabled="busy || !creating"
            :aria-invalid="!!idError"
            class="font-mono"
            @input="idTouched = true"
          />
          <FieldError v-if="idError && form.id">{{ idError }}</FieldError>
          <FieldDescription v-else>{{
            creating
              ? tr(
                  "Stabile Kennung; später nicht änderbar.",
                  "Stable identifier; cannot change later.",
                )
              : tr(
                  "Die Task-ID kann nicht geändert werden.",
                  "Task ID cannot be changed.",
                )
          }}</FieldDescription>
        </Field>
      </FieldGroup>
      <Field orientation="horizontal">
        <Switch id="task-enabled" v-model="form.enabled" :disabled="busy" />
        <FieldLabel for="task-enabled">{{
          tr(
            "Planmäßig und per Eingang ausführen",
            "Run on schedule and on input",
          )
        }}</FieldLabel>
      </Field>
    </FieldSet>

    <FieldSet>
      <FieldLegend>{{ tr("Ausführung", "Execution") }}</FieldLegend>
      <FieldGroup class="grid gap-4 md:grid-cols-3">
        <Field>
          <FieldLabel for="task-interval">{{
            tr("Intervall (Sekunden)", "Interval (seconds)")
          }}</FieldLabel>
          <Input
            id="task-interval"
            v-model="form.intervalSeconds"
            type="number"
            min="0"
            max="31536000"
            :disabled="busy"
          />
          <FieldDescription>{{
            tr("0 = nur manuell oder per Eingang.", "0 = manual or input only.")
          }}</FieldDescription>
        </Field>
        <Field>
          <FieldLabel for="task-timeout">{{
            tr("Zeitlimit (Sekunden)", "Timeout (seconds)")
          }}</FieldLabel>
          <Input
            id="task-timeout"
            v-model="form.timeoutSeconds"
            type="number"
            min="1"
            :max="limits?.maximumTimeoutSeconds ?? 3600"
            :disabled="busy"
          />
        </Field>
        <Field>
          <FieldLabel for="task-memory">{{
            tr("Speicher (MB)", "Memory (MB)")
          }}</FieldLabel>
          <Input
            id="task-memory"
            v-model="form.memoryMb"
            type="number"
            min="32"
            :max="limits?.maximumMemoryMb ?? 2048"
            :disabled="busy"
          />
        </Field>
      </FieldGroup>
    </FieldSet>

    <Field>
      <FieldLabel for="task-script">JavaScript (ESM)</FieldLabel>
      <Textarea
        id="task-script"
        wrap="off"
        v-model="form.script"
        class="min-h-80 font-mono text-sm"
        spellcheck="false"
        :disabled="busy"
      />
      <FieldDescription
        >export default async ({ config, secrets, state, input, signal }) =&gt;
        ({ data, state?, events? })</FieldDescription
      >
    </Field>

    <FieldSet>
      <FieldLegend>{{ tr("Aufbewahrung", "Retention") }}</FieldLegend>
      <FieldGroup class="grid gap-4 md:grid-cols-3">
        <Field>
          <FieldLabel for="task-retention-count">{{
            tr("Ergebnisse behalten", "Keep results")
          }}</FieldLabel>
          <Input
            id="task-retention-count"
            v-model="form.retention.maximumCount"
            type="number"
            min="1"
            :disabled="busy"
          />
        </Field>
        <Field>
          <FieldLabel for="task-retention-days">{{
            tr("Höchstalter (Tage)", "Maximum age (days)")
          }}</FieldLabel>
          <Input
            id="task-retention-days"
            v-model="form.retention.maximumAgeDays"
            type="number"
            min="1"
            :disabled="busy"
          />
        </Field>
        <Field>
          <FieldLabel for="task-retention-size">{{
            tr("Höchstgröße (MB)", "Maximum size (MB)")
          }}</FieldLabel>
          <Input
            id="task-retention-size"
            v-model="form.retentionMb"
            type="number"
            min="1"
            :disabled="busy"
          />
        </Field>
      </FieldGroup>
    </FieldSet>

    <Disclosure
      variant="card"
      :title="
        tr(
          'Erweitert: Konfiguration, Secrets, Eingang, Module',
          'Advanced: configuration, secrets, input, modules',
        )
      "
    >
      <FieldGroup class="gap-5">
        <Field :data-invalid="!!configState.error || undefined">
          <FieldLabel for="task-config">{{
            tr("Konfiguration (JSON)", "Configuration (JSON)")
          }}</FieldLabel>
          <Textarea
            id="task-config"
            wrap="off"
            v-model="config"
            class="min-h-32 font-mono text-sm"
            spellcheck="false"
            :disabled="busy"
            :aria-invalid="!!configState.error"
          />
          <FieldError v-if="configState.error">{{
            configState.error
          }}</FieldError>
          <FieldDescription v-else>{{
            tr(
              "Wird dem Script als config übergeben. Keine Zugangsdaten hier ablegen.",
              "Passed to the script as config. Never store credentials here.",
            )
          }}</FieldDescription>
        </Field>
        <Field>
          <FieldLabel for="task-secrets">{{
            tr("Secrets", "Secrets")
          }}</FieldLabel>
          <Input
            id="task-secrets"
            v-model="form.secretList"
            class="font-mono"
            :placeholder="
              tr(
                'z. B. API_TOKEN, ACCOUNT_PASSWORD',
                'e.g. API_TOKEN, ACCOUNT_PASSWORD',
              )
            "
            :disabled="busy"
          />
          <FieldDescription>{{
            tr(
              "Namen der Dienst-Secrets, die dieser Task erhält.",
              "Names of service secrets this task receives.",
            )
          }}</FieldDescription>
        </Field>
        <FieldGroup class="grid gap-4 md:grid-cols-2">
          <Field>
            <FieldLabel for="task-input-secret">{{
              tr("Secret für HTTP-Eingang", "HTTP input secret")
            }}</FieldLabel>
            <Input
              id="task-input-secret"
              v-model="form.inputSecret"
              class="font-mono"
              :disabled="busy"
            />
          </Field>
          <Field orientation="horizontal" class="self-end">
            <Switch
              id="task-open-input"
              v-model="form.allowUnauthenticated"
              :disabled="busy || !!form.inputSecret"
            />
            <FieldLabel for="task-open-input">{{
              tr(
                "Eingang ohne Anmeldung erlauben",
                "Allow unauthenticated input",
              )
            }}</FieldLabel>
          </Field>
        </FieldGroup>
        <Field :data-invalid="!!dependencyState.error || undefined">
          <FieldLabel for="task-dependencies">{{
            tr("Node-Module (JSON)", "Node modules (JSON)")
          }}</FieldLabel>
          <Textarea
            id="task-dependencies"
            wrap="off"
            v-model="dependencies"
            class="min-h-24 font-mono text-sm"
            spellcheck="false"
            :disabled="busy"
            :aria-invalid="!!dependencyState.error"
          />
          <FieldError v-if="dependencyState.error">{{
            dependencyState.error
          }}</FieldError>
          <FieldDescription v-else>{{
            tr(
              'Exakte Versionen, z. B. {"date-fns": "4.1.0"}.',
              'Exact versions, e.g. {"date-fns": "4.1.0"}.',
            )
          }}</FieldDescription>
        </Field>
      </FieldGroup>
    </Disclosure>

    <div class="flex flex-wrap gap-2">
      <Button type="submit" :disabled="busy || !valid">{{
        tr("Speichern", "Save")
      }}</Button>
      <Button
        type="button"
        variant="outline"
        :disabled="busy"
        @click="emit('cancel')"
        >{{ tr("Abbrechen", "Cancel") }}</Button
      >
    </div>
  </form>
</template>
