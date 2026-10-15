<script setup lang="ts">
import { computed, onMounted, ref } from "vue";
import {
  Button,
  Field,
  FieldDescription,
  FieldError,
  FieldLabel,
  Input,
  SettingsSection,
  Switch,
} from "@ivy/ui";
import { newOperationId } from "../../../../packages/sdk/src/client.js";
import type { Operation } from "../../../../packages/sdk/src/client.js";
import { localeText as tr } from "../../../../packages/ui-client/src/runtime";
import { consoleClient } from "./runtime";

const enabled = ref(false),
  start = ref("23:00"),
  end = ref("07:00");
const timeZone = ref(Intl.DateTimeFormat().resolvedOptions().timeZone);
const revision = ref(0),
  baseline = ref(""),
  loaded = ref(false),
  busy = ref(false);
const pending = ref<Operation.SettingsSaveParams | null>(null);
const error = ref(""),
  message = ref("");
const settings = computed<Operation.HiveSettings>(() => ({
  schemaVersion: 1,
  quietHours: enabled.value
    ? { start: start.value, end: end.value, timeZone: timeZone.value.trim() }
    : null,
}));
const dirty = computed(() => JSON.stringify(settings.value) !== baseline.value);
function adopt(saved: Operation.HiveSettingsSnapshot) {
  revision.value = saved.revision;
  enabled.value = !!saved.settings.quietHours;
  if (saved.settings.quietHours)
    ({
      start: start.value,
      end: end.value,
      timeZone: timeZone.value,
    } = saved.settings.quietHours);
  baseline.value = JSON.stringify(saved.settings);
  loaded.value = true;
}
async function load() {
  if (busy.value || pending.value) return;
  busy.value = true;
  error.value = "";
  message.value = "";
  try {
    adopt(await consoleClient.request("settings.get", {}));
  } catch (failure) {
    error.value = failure instanceof Error ? failure.message : String(failure);
  } finally {
    busy.value = false;
  }
}
async function save() {
  if (busy.value || !loaded.value) return;
  busy.value = true;
  error.value = "";
  message.value = "";
  try {
    pending.value ??= {
      mutationId: await newOperationId(consoleClient),
      expectedRevision: revision.value,
      settings: settings.value,
    };
    adopt(await consoleClient.request("settings.save", pending.value));
    pending.value = null;
    message.value = tr(
      "Nachtruhe im Hive gespeichert.",
      "Quiet hours saved in Hive.",
    );
  } catch (failure) {
    const code = (failure as { code?: string }).code;
    if (
      [
        "revision_conflict",
        "invalid_arguments",
        "target_conflict",
        "forbidden",
      ].includes(code ?? "")
    )
      pending.value = null;
    error.value =
      code === "revision_conflict"
        ? tr(
            "Die Einstellung wurde inzwischen geändert. Lade sie neu und speichere erneut.",
            "The setting has changed. Reload it before saving again.",
          )
        : failure instanceof Error
          ? failure.message
          : String(failure);
  } finally {
    busy.value = false;
  }
}
onMounted(load);
</script>

<template>
  <SettingsSection
    class="mb-6"
    :title="tr('Nachtruhe', 'Quiet hours')"
    :description="
      tr(
        'Automatische Hinweise und Anrufe pausieren in diesem Zeitraum. Die Einstellung gilt für den gesamten Hive.',
        'Automatic notices and calls pause during this period. This setting applies throughout Hive.',
      )
    "
  >
    <form class="space-y-4" @submit.prevent="save">
      <Field orientation="horizontal">
        <FieldLabel for="quiet-hours-enabled" class="flex-1">{{
          tr("Nachtruhe aktivieren", "Enable quiet hours")
        }}</FieldLabel>
        <Switch
          id="quiet-hours-enabled"
          v-model="enabled"
          :disabled="busy || !loaded || !!pending"
        />
      </Field>
      <div v-if="enabled" class="grid gap-4 sm:grid-cols-2">
        <Field>
          <FieldLabel for="quiet-hours-start">{{
            tr("Beginn", "Start")
          }}</FieldLabel>
          <Input
            id="quiet-hours-start"
            v-model="start"
            type="time"
            required
            :disabled="busy || !!pending"
          />
        </Field>
        <Field>
          <FieldLabel for="quiet-hours-end">{{ tr("Ende", "End") }}</FieldLabel>
          <Input
            id="quiet-hours-end"
            v-model="end"
            type="time"
            required
            :disabled="busy || !!pending"
          />
        </Field>
        <Field class="sm:col-span-2">
          <FieldLabel for="quiet-hours-zone">{{
            tr("Zeitzone", "Time zone")
          }}</FieldLabel>
          <Input
            id="quiet-hours-zone"
            v-model="timeZone"
            required
            :disabled="busy || !!pending"
          />
          <FieldDescription>{{
            tr(
              "Die Zeiten gelten in dieser Zeitzone, einschließlich Sommer- und Winterzeit.",
              "Times follow this time zone, including daylight saving changes.",
            )
          }}</FieldDescription>
        </Field>
      </div>
      <div class="flex flex-wrap items-center gap-3">
        <Button
          type="submit"
          :disabled="busy || !loaded || (!pending && !dirty)"
          >{{
            pending
              ? tr("Speichern erneut versuchen", "Retry original save")
              : tr("Speichern", "Save")
          }}</Button
        >
        <Button
          type="button"
          variant="outline"
          :disabled="busy || !!pending"
          @click="load"
          >{{ tr("Neu laden", "Reload") }}</Button
        >
      </div>
      <p v-if="message" role="status" class="text-sm">{{ message }}</p>
      <FieldError v-if="error">{{ error }}</FieldError>
    </form>
  </SettingsSection>
</template>
