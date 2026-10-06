<script setup lang="ts">
import { Alert, AlertDescription, AlertTitle, Button, StatusBadge } from '@ivy/ui';
import type { SavedNativeAction } from './native-action';
defineProps<{ action: SavedNativeAction | null; busy: boolean; error: string | null }>();
defineEmits<{ reconcile: []; retry: [] }>();
</script>
<template>
  <Alert v-if="action || error" :variant="error ? 'destructive' : 'default'" class="mt-3" aria-live="polite">
    <AlertTitle v-if="action" class="flex flex-wrap items-center gap-2"><span>{{ action.label }}</span><StatusBadge :label="action.phase" :tone="action.phase === 'succeeded' ? 'good' : action.phase === 'failed' ? 'bad' : 'warning'" /></AlertTitle>
    <AlertDescription class="space-y-2">
      <p v-if="error" class="text-destructive" role="alert">{{ error }}</p>
      <template v-if="action">
        <p class="text-foreground">{{ action.detail }}</p><p class="font-mono text-xs break-all">Operation {{ action.operationId }} · {{ action.call.serviceNodeId }}</p>
        <div class="flex flex-wrap gap-2"><Button variant="outline" size="sm" :disabled="busy" @click="$emit('reconcile')">Check original outcome</Button><Button v-if="action.phase === 'not_found'" size="sm" :disabled="busy || action.secret" @click="$emit('retry')">Retry original request</Button></div>
        <p v-if="action.secret && action.phase === 'not_found'">Re-enter the original secret response in the form. Sending it retains this operation identity.</p>
      </template>
    </AlertDescription>
  </Alert>
</template>
