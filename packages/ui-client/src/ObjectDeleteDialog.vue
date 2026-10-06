<script setup lang="ts">
import { ref } from 'vue';
import { Button, Dialog, DialogContent, DialogDescription, DialogFooter, DialogTitle } from '@ivy/ui';
import { IvyError, newOperationId } from '../../sdk/src/client.js';
import type { RpcClient } from '../../sdk/src/client.js';

interface PendingDelete { objectId: string; expectedRevision: number; mutationId: string }
const props = defineProps<{
  client: RpcClient;
  objectId: string;
  expectedRevision: number | null;
  label: string;
  consequence: string;
  storageKey: string;
}>();
const emit = defineEmits<{ deleted: [] }>();
const open = defineModel<boolean>('open', { default: false });
const pending = ref<PendingDelete | null>(null), busy = ref(false), error = ref('');
try {
  const saved = JSON.parse(sessionStorage.getItem(props.storageKey) ?? 'null') as Partial<PendingDelete> | null;
  if (saved?.objectId === props.objectId && Number.isSafeInteger(saved.expectedRevision) && typeof saved.mutationId === 'string') {
    pending.value = saved as PendingDelete;
    open.value = true;
  }
} catch { error.value = 'The previous deletion request could not be restored.'; }
const remove = async () => {
  if (busy.value || (!pending.value && props.expectedRevision === null)) return;
  busy.value = true; error.value = '';
  try {
    if (!pending.value) {
      pending.value = {
        objectId: props.objectId,
        expectedRevision: props.expectedRevision!,
        mutationId: await newOperationId(props.client),
      };
      sessionStorage.setItem(props.storageKey, JSON.stringify(pending.value));
    }
    await props.client.request('objects.delete', pending.value);
    pending.value = null;
    sessionStorage.removeItem(props.storageKey);
    open.value = false;
    emit('deleted');
  } catch (cause) {
    if (IvyError.from(cause).outcome === 'not_executed') {
      pending.value = null;
      sessionStorage.removeItem(props.storageKey);
    }
    error.value = cause instanceof Error ? cause.message : 'Deletion could not be confirmed. Retry the original request.';
  } finally { busy.value = false; }
};
</script>

<template>
  <Dialog v-model:open="open">
    <DialogContent>
      <DialogTitle>Are you sure you want to permanently delete {{ label }}?</DialogTitle>
      <DialogDescription>{{ consequence }} All revisions will be removed. This cannot be undone.</DialogDescription>
      <p v-if="error" role="alert" class="text-sm text-destructive">{{ error }}</p>
      <p v-if="pending" class="text-sm text-muted-foreground">The previous result is unconfirmed. Retrying uses the same deletion request.</p>
      <DialogFooter>
        <Button variant="outline" :disabled="busy" @click="open = false">Cancel</Button>
        <Button variant="destructive" :disabled="busy || (!pending && expectedRevision === null)" @click="remove">
          {{ busy ? 'Deleting…' : pending ? 'Retry deletion' : 'Delete permanently' }}
        </Button>
      </DialogFooter>
    </DialogContent>
  </Dialog>
</template>
