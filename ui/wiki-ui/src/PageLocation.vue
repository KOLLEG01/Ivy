<script setup lang="ts">
import { computed, ref } from 'vue';
import { Button, Dialog, DialogContent, DialogDescription, DialogTitle, HierarchyTree, Input, Label } from '@ivy/ui';
import { objectTreeLoader } from '../../../packages/ui-client/src/object-tree';
import { IvyError, route } from '../../../packages/ui-client/src/runtime';
import type { Operation } from '../../../packages/ui-client/src/runtime';
import { base, client } from './runtime';
import { newOperationId } from '../../../packages/sdk/src/client';
const props = defineProps<{ page: Operation.ObjectMetadata; disabled?: boolean; hideTrigger?: boolean }>();
const emit = defineEmits<{ changed: [] }>();
const open = ref(false), busy = ref(false), error = ref(''), parentLabel = ref('');
const draft = ref({ name: props.page.name, parentId: props.page.parentId, icon: props.page.icon ?? '', mutationId: '' });
const key = 'ivy.wiki.location:' + base.href + props.page.id;
try { const saved = JSON.parse(sessionStorage.getItem(key) ?? 'null'); if (saved && typeof saved.name === 'string' && (saved.parentId === null || typeof saved.parentId === 'string') && typeof saved.icon === 'string' && typeof saved.mutationId === 'string') { draft.value = saved; open.value = !!saved.mutationId; } } catch { /* A new explicit location is still possible. */ }
const treeLoader = objectTreeLoader(client, id => route('page', { id }), 'wiki/page');
const load: typeof treeLoader = async (...args) => { const result = await treeLoader(...args); return { ...result, items: result.items.filter(item => item.id !== props.page.id) }; };
const locked = computed(() => busy.value || !!draft.value.mutationId);
const begin = () => { if (!draft.value.mutationId) { draft.value = { name: props.page.name, parentId: props.page.parentId, icon: props.page.icon ?? '', mutationId: '' }; parentLabel.value = props.page.parentId ? 'Current parent' : 'Wiki root'; } open.value = true; };
const save = async () => {
  if (busy.value || !draft.value.name.trim()) return; busy.value = true; error.value = '';
  try {
    if (!draft.value.mutationId) {
      const current = await client.request('objects.stat', { objectId: props.page.id });
      if (current.effectivelyArchived || current.contractKey !== 'wiki/page' || current.contractVersion !== '1.0.0') throw new Error('This page is read-only.');
      if (draft.value.parentId) { const parent = await client.request('objects.stat', { objectId: draft.value.parentId }); if (parent.effectivelyArchived || parent.contractKey !== 'wiki/page' || parent.contractVersion !== '1.0.0') throw new Error('Choose a writable Wiki page.'); }
      draft.value.mutationId = await newOperationId(client);
    }
    sessionStorage.setItem(key, JSON.stringify(draft.value));
    await client.request('objects.move', { objectId: props.page.id, ...draft.value, icon: draft.value.icon.trim() || null });
    sessionStorage.removeItem(key); draft.value.mutationId = ''; open.value = false; emit('changed'); window.dispatchEvent(new Event('ivy:objects-changed'));
  } catch (cause) { error.value = cause instanceof Error ? cause.message : 'The page move could not be confirmed.'; if (cause instanceof IvyError && cause.outcome === 'not_executed') { draft.value.mutationId = ''; sessionStorage.removeItem(key); } }
  finally { busy.value = false; }
};
defineExpose({ open: begin });
</script>
<template>
  <Button v-if="!hideTrigger || draft.mutationId" variant="ghost" size="sm" :disabled="disabled && !draft.mutationId" @click="begin">{{ draft.mutationId ? 'Resolve page move' : 'Rename or move' }}</Button>
  <Dialog v-model:open="open"><DialogContent class="max-h-[calc(100dvh-2rem)] overflow-y-auto"><DialogTitle>Page settings</DialogTitle><DialogDescription>Set this page's icon, title and place in your Wiki.</DialogDescription>
    <form class="space-y-4" @submit.prevent="save"><div><Label for="location-icon">Page icon</Label><Input id="location-icon" v-model="draft.icon" class="mt-2 w-24" maxlength="32" placeholder="📄" :disabled="locked" /><p class="mt-1 text-xs text-muted-foreground">Optional emoji or short symbol.</p></div><div><Label for="location-title">Page title</Label><Input id="location-title" v-model="draft.name" class="mt-2" maxlength="255" required :disabled="locked" /></div>
      <div><p class="mb-2 text-sm font-medium">Parent page</p><Button variant="outline" type="button" :disabled="locked" @click="draft.parentId = null; parentLabel = 'Wiki root'">Wiki root</Button><p role="status" class="my-3 text-sm text-muted-foreground">{{ draft.parentId ? parentLabel || 'Saved parent selection' : 'Wiki root' }}</p>
        <div v-if="!locked" class="max-h-64 overflow-auto rounded-md border p-2"><HierarchyTree :live-scopes="['objects']" label="Choose parent page" :storage-key="'ivy.wiki.move-tree:' + base.href" :load="load" :selected-id="draft.parentId" select-only @activate="draft.parentId = $event.id; parentLabel = $event.label" /></div>
      </div>
      <p v-if="error" role="alert" class="text-sm">{{ error }}</p><p v-if="draft.mutationId" class="text-sm text-muted-foreground">The result is unconfirmed. Retrying keeps the exact icon, title and parent.</p>
      <div class="flex justify-end gap-2"><Button type="button" variant="ghost" @click="open = false">Close</Button><Button type="submit" :disabled="busy || !draft.name.trim()">{{ busy ? 'Saving…' : draft.mutationId ? 'Retry original move' : 'Save location' }}</Button></div>
    </form>
  </DialogContent></Dialog>
</template>
