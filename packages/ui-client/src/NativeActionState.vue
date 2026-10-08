<script setup lang="ts">
import { computed, onBeforeUnmount, ref, watch } from "vue";
import { Alert, AlertDescription, AlertTitle, Button } from "@ivy/ui";
import type { SavedNativeAction } from "./native-action";
import { localeText as tr } from "./runtime";
const props = defineProps<{ action: SavedNativeAction | null; busy: boolean; error: string | null }>();
defineEmits<{ reconcile: []; retry: [] }>();
const overdue = ref(false);
let timer: ReturnType<typeof setTimeout> | undefined;
watch(() => [props.action?.operationId, props.action?.createdAt], () => {
  clearTimeout(timer);
  const started = Date.parse(props.action?.createdAt ?? "");
  const remaining = started + 120000 - Date.now();
  overdue.value = !!props.action && (!Number.isFinite(remaining) || remaining <= 0);
  if (remaining > 0) timer = setTimeout(() => overdue.value = true, remaining);
}, { immediate: true });
onBeforeUnmount(() => clearTimeout(timer));
const failed = computed(() => props.action?.phase === "failed" || props.action?.phase === "not_found");
const unresolved = computed(() => props.action && ["prepared", "unknown", "pending"].includes(props.action.phase));
const visible = computed(() => failed.value || (overdue.value && unresolved.value) || (!!props.error && !unresolved.value));
const unconfirmed = computed(() => overdue.value && unresolved.value && props.action?.phase !== "pending");
const message = computed(() => {
  if (props.action?.phase === "not_found") return tr("Die Anfrage wurde nicht angenommen. Du kannst sie erneut versuchen.", "The request was not accepted. You can retry it.");
  if (props.action?.phase === "failed") return props.action.detail;
  if (unresolved.value) return props.action?.phase === "pending"
    ? tr("Die Anfrage wird noch bearbeitet. Das dauert länger als üblich. Der Status wird weiter geprüft.", "The request is still being processed and is taking longer than usual. Its status is being checked.")
    : tr("Die Anfrage konnte bisher nicht bestätigt werden. Der Status wird weiter geprüft.", "The request has not been confirmed yet. Its status is being checked.");
  return props.error;
});
</script>
<template>
  <Alert v-if="visible" :variant="failed || unconfirmed || error && !unresolved ? 'destructive' : 'default'" class="mt-3" aria-live="polite">
    <AlertTitle>{{ failed || error && !unresolved ? tr('Anfrage fehlgeschlagen', 'Request failed') : tr('Die Anfrage dauert länger', 'The request is taking longer') }}</AlertTitle>
    <AlertDescription class="space-y-2">
      <p>{{ message }}</p>
      <div v-if="unresolved || action?.phase === 'not_found'" class="flex flex-wrap gap-2">
        <Button variant="outline" size="sm" :disabled="busy" :loading="busy" @click="$emit('reconcile')">{{ tr('Status prüfen', 'Check status') }}</Button>
        <Button v-if="action?.phase === 'not_found'" size="sm" :disabled="busy || action.secret" @click="$emit('retry')">{{ tr('Erneut versuchen', 'Retry request') }}</Button>
      </div>
      <p v-if="action?.secret && action.phase === 'not_found'">{{ tr('Gib die ursprüngliche vertrauliche Antwort erneut im Formular ein.', 'Re-enter the original secret response in the form.') }}</p>
    </AlertDescription>
  </Alert>
</template>
