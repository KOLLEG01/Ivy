<script setup lang="ts">
import { computed, ref, watch } from 'vue';
import { Alert, AlertDescription, Button, Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle,
  DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuTrigger, Field, Input, Label } from '@ivy/ui';
import { Pencil, Trash2 } from '@lucide/vue';
import type { Agent, RpcClient } from '../../sdk/src/client.js';
import { discover } from '../../sdk/src/client.js';
import { useNativeAction } from './native-action';

const props = defineProps<{ client: RpcClient; node: string; project: Agent.ProjectSummary }>();
const emit = defineEmits<{ changed: [] }>();
const mode = ref<'rename' | 'remove'>('rename'), open = ref(false), name = ref('');
const action = useNativeAction(props.client, 'ivy:project-action:' + props.node + ':' + props.project.nativeId);
const message = computed(() => action.error.value || (action.saved.value?.phase === 'failed' ? action.saved.value.detail : ''));
const choose = (value: 'rename' | 'remove') => { mode.value = value; name.value = props.project.name; open.value = true; };
async function apply() {
  if (action.locked.value) return;
  try {
    const method = mode.value === 'rename' ? 'project/update' : 'project/delete';
    const binding = await discover(props.client, 'codex.' + method, { serviceNodeId: props.node });
    await action.start(mode.value === 'rename' ? 'Rename project' : 'Remove project', binding,
      { projectId: props.project.nativeId, ...(mode.value === 'rename' ? { name: name.value.trim() } : {}) });
  } catch (cause) { action.error.value = cause instanceof Error ? cause.message : String(cause); }
}
watch(() => action.saved.value?.phase, phase => { if (phase === 'succeeded') { open.value = false; emit('changed'); } });
</script>

<template>
  <DropdownMenu>
    <DropdownMenuTrigger as-child><slot /></DropdownMenuTrigger>
    <DropdownMenuContent align="end">
      <DropdownMenuItem @select="choose('rename')"><Pencil aria-hidden="true" />Rename project</DropdownMenuItem>
      <DropdownMenuItem @select="choose('remove')"><Trash2 aria-hidden="true" />Remove project</DropdownMenuItem>
    </DropdownMenuContent>
  </DropdownMenu>
  <Dialog v-model:open="open">
    <DialogContent>
      <DialogHeader>
        <DialogTitle>{{ mode === 'rename' ? 'Rename project' : 'Remove project' }}</DialogTitle>
        <DialogDescription>{{ mode === 'rename' ? 'Choose the project name shown by Codex.' : 'Remove “' + project.name + '” from Codex. Files in its working directories are kept.' }}</DialogDescription>
      </DialogHeader>
      <Field v-if="mode === 'rename'">
        <Label :for="'project-name-' + project.nativeId">Project name</Label>
        <Input :id="'project-name-' + project.nativeId" v-model="name" :disabled="action.locked.value" @keydown.enter="apply" />
      </Field>
      <Alert v-if="message" variant="destructive"><AlertDescription>{{ message }}</AlertDescription></Alert>
      <Alert v-if="action.locked.value && !action.busy.value">
        <AlertDescription>The result is not yet confirmed.
          <Button variant="link" @click="action.reconcile">Check result</Button>
          <Button v-if="action.saved.value?.phase === 'not_found'" variant="link" @click="action.retry">Retry</Button>
        </AlertDescription>
      </Alert>
      <DialogFooter>
        <Button variant="outline" @click="open = false">Cancel</Button>
        <Button :variant="mode === 'remove' ? 'destructive' : 'default'" :disabled="action.locked.value || (mode === 'rename' && !name.trim())" @click="apply">
          {{ mode === 'rename' ? 'Save name' : 'Remove project' }}
        </Button>
      </DialogFooter>
    </DialogContent>
  </Dialog>
</template>
