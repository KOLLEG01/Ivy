<script setup lang="ts">
import { computed, useId } from 'vue';
import { Disclosure, Button, Input, Label, StatusBadge } from '@ivy/ui';
import type { useAction } from './action';
import { statusTone, tr } from './runtime';
const props = defineProps<{ action: ReturnType<typeof useAction> }>();
const emit = defineEmits<{ changed: [] }>();
const fileId = useId();
const problem = computed(() => !props.action.busy.value && (!!props.action.error.value ||
  !props.action.identityMatches.value || !!props.action.saved.value && props.action.saved.value.phase !== 'succeeded'));
const message = computed(() => {
  const action = props.action;
  if (!action.identityMatches.value) return tr('Diese Änderung gehört zum ursprünglichen Konto und Arbeitsbereich. Bitte dort anmelden, um sie abzugleichen.',
    'This change belongs to the original account and workspace. Sign back in there to resolve it.');
  const failure = action.error.value || (action.saved.value?.phase === 'failed' ? action.saved.value.detail : '');
  if (failure) return failure.replace(/\s+\([a-z_]+;\s*(?:unknown|not_executed|completed)\)$/, '');
  return action.saved.value?.phase === 'prepared'
    ? tr('Diese Änderung wurde noch nicht gesendet. Deine Eingabe bleibt erhalten.', 'This change has not been sent. Your input is retained.')
    : tr('Diese Änderung ist noch nicht bestätigt. Sie wird im Hintergrund geprüft.', 'This change is not confirmed yet. It is being checked in the background.');
});
const check = async (resume = false) => {
  if (resume) await props.action.replay(); else await props.action.reconcile();
  if (!props.action.busy.value && !props.action.error.value && props.action.identityMatches.value && props.action.saved.value?.phase === 'succeeded') emit('changed');
};
const restore = async (event: Event) => {
  const input = event.target as HTMLInputElement, file = input.files?.[0];
  if (file) await props.action.restoreFile(file);
  input.value = '';
};
</script>
<template><div v-if="problem" class="mb-4 space-y-2 text-sm" aria-live="polite">
  <p :role="action.error.value || action.saved.value?.phase === 'failed' ? 'alert' : 'status'" :class="action.error.value || action.saved.value?.phase === 'failed' ? 'text-destructive' : 'text-muted-foreground'">{{ message }}</p>
  <div v-if="action.needsOriginalFile.value && action.identityMatches.value" class="space-y-2">
    <Label :for="fileId">Original file</Label><Input :id="fileId" type="file" :disabled="action.busy.value" @change="restore" />
  </div>
  <div v-if="action.saved.value && action.identityMatches.value && !['failed', 'succeeded'].includes(action.saved.value.phase)" class="flex flex-wrap gap-2">
    <Button type="button" size="sm" variant="outline" :disabled="action.busy.value" @click="check()">{{ tr('Erneut prüfen', 'Check again') }}</Button>
    <Button v-if="action.saved.value.phase === 'prepared'" type="button" size="sm" variant="outline" :disabled="action.busy.value" @click="check(true)">{{ tr('Änderung fortsetzen', 'Continue change') }}</Button>
  </div>
  <Button v-if="action.canRetryPreparation.value" type="button" size="sm" variant="outline" @click="action.retryPreparation">{{ tr('Erneut versuchen', 'Try again') }}</Button>
  <Button v-if="action.canDismiss.value" type="button" size="sm" variant="ghost" @click="action.dismiss">{{ tr('Hinweis schließen', 'Dismiss message') }}</Button>
  <Disclosure v-if="action.saved.value" :title="tr('Details', 'Details')">
    <div class="flex flex-wrap items-center gap-2"><span>{{ action.saved.value.label }}</span><StatusBadge :label="action.saved.value.phase" :tone="statusTone(action.saved.value.phase)" /></div>
    <p class="mt-1 text-muted-foreground">{{ action.saved.value.detail }}</p>
    <p class="mt-1 font-mono text-xs break-all text-muted-foreground">{{ action.saved.value.call.operationId }} · {{ action.saved.value.call.serviceNodeId }}</p>
    <Button v-if="action.identityMatches.value && !['failed', 'succeeded', 'prepared'].includes(action.saved.value.phase)" type="button" class="mt-2" size="sm" variant="outline" @click="check(true)">{{ tr('Ursprüngliche Änderung fortsetzen', 'Continue original change') }}</Button>
  </Disclosure>
</div></template>
