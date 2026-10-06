<script setup lang="ts">
import { Disclosure, Button, StatusBadge } from '@ivy/ui';
import type { useAction } from './action';
import { statusTone } from './runtime';
defineProps<{ action: ReturnType<typeof useAction> }>();
const emit = defineEmits<{ changed: [] }>();
</script>
<template><div v-if="action.saved.value || action.error.value" class="mb-4 space-y-2 rounded-lg border bg-card p-3 text-sm" aria-live="polite">
  <div v-if="action.saved.value" class="flex flex-wrap items-center gap-2"><span class="font-medium">{{ action.saved.value.label }}</span><StatusBadge :label="action.saved.value.phase" :tone="statusTone(action.saved.value.phase)" /></div>
  <p v-if="action.saved.value" class="text-muted-foreground">{{ action.saved.value.detail }}</p><p v-if="action.error.value" role="alert" class="text-destructive">{{ action.error.value }}</p>
  <p v-if="!action.identityMatches.value" class="text-muted-foreground">This action belongs to the original caller and workspace. Sign back in there to resolve it.</p>
  <div v-if="action.saved.value && action.identityMatches.value && !['failed', 'succeeded'].includes(action.saved.value.phase)" class="flex flex-wrap gap-2">
    <Button type="button" size="sm" variant="outline" :disabled="action.busy.value" @click="async () => { await action.reconcile(); emit('changed'); }">Check original action</Button>
    <Button type="button" size="sm" variant="outline" :disabled="action.busy.value" @click="async () => { await action.replay(); emit('changed'); }">Resume original action</Button>
  </div>
  <Button v-if="action.canRetryPreparation.value" type="button" size="sm" variant="outline" @click="action.retryPreparation">Retry preparation</Button>
  <Disclosure v-if="action.saved.value" title="Action identity"><p class="mt-1 font-mono text-xs break-all text-muted-foreground">{{ action.saved.value.call.operationId }} · {{ action.saved.value.call.serviceNodeId }}</p></Disclosure>
</div></template>
