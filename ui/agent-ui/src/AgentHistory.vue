<script setup lang="ts">
import { ref, watch } from 'vue';
import { Button, Empty, EmptyDescription, EmptyHeader, EmptyTitle, Item, ItemActions, ItemContent, ItemDescription,
  ItemGroup, ItemTitle, Label, OptionSelect, PageControls, RemoteState, useRemote } from '@ivy/ui';
import { ArrowRight } from '@lucide/vue';
import type { Agent } from '../../../packages/sdk/src/client.js';
import { nativeThreadPage } from '../../../packages/ui-client/src/native-thread-pages';
import { nativeRouteQuery } from '../../../packages/ui-client/src/native-route';
import { route } from '../../../packages/ui-client/src/runtime';
import { client, tr } from './runtime';

const props = defineProps<{ node: string; backend?: Agent.BackendIdentity | undefined }>();
const owner = { serviceNodeId: props.node, namespace: props.backend?.backendId ?? 'codex', backend: props.backend };
const archive = ref('current');
const position = ref<{ page: number; cursor: string | undefined }>({ page: 1, cursor: undefined });
// Retain page addresses, never preceding page bodies. Back navigation is bounded;
// the first-page action remains available at any depth in the native history.
const previous = ref<Array<typeof position.value>>([]);
const history = useRemote(signal => nativeThreadPage(client, owner,
  { archived: archive.value === 'archived', cursor: position.value.cursor }, signal), 0,
  ['inventory/' + owner.namespace + '/thread/' + props.node, 'services/agent-manager/' + props.node]);
const first = () => {
  position.value = { page: 1, cursor: undefined };
  previous.value = [];
  history.value.value = null;
  void history.refresh();
};
const next = () => {
  if (history.loading.value || !history.value.value?.nextCursor || history.error.value) return;
  previous.value.push(position.value);
  if (previous.value.length > 32) previous.value.shift();
  position.value = { page: position.value.page + 1, cursor: history.value.value.nextCursor };
  history.value.value = null;
  void history.refresh();
};
const back = () => {
  if (history.loading.value || !previous.value.length) return;
  position.value = previous.value.pop()!;
  history.value.value = null;
  void history.refresh();
};
watch(archive, first);
</script>

<template>
  <section :aria-label="tr('Aufgabenverlauf', 'Task history')" class="space-y-4">
    <div class="flex flex-wrap items-end justify-between gap-3">
      <div class="min-w-0">
        <Label for="agent-history-archive">{{ tr('Aufgaben', 'Tasks') }}</Label>
        <OptionSelect id="agent-history-archive" v-model="archive" class="mt-2 w-full">
          <option value="current">{{ tr('Aktuell', 'Current') }}</option>
          <option value="archived">{{ tr('Archiviert', 'Archived') }}</option>
        </OptionSelect>
      </div>
      <Button variant="outline" size="sm" :disabled="history.loading.value" @click="first">{{ tr('Neueste Aufgaben', 'Latest tasks') }}</Button>
    </div>
    <RemoteState :loading="history.loading.value" :error="history.error.value" :has-data="history.value.value !== null" @retry="history.refresh" />
    <ItemGroup v-if="history.value.value?.items.length">
      <Item v-for="item in history.value.value.items" :key="item.id" as-child variant="outline" class="min-w-0 rounded-xl">
        <a :href="route('task', { ...nativeRouteQuery(owner), id: item.id })" :aria-label="item.name || item.preview || item.id">
          <ItemContent class="min-w-0">
            <ItemTitle class="break-all">{{ item.name || item.preview || item.id }}</ItemTitle>
            <ItemDescription class="break-all">{{ item.cwd }}</ItemDescription>
          </ItemContent>
          <ItemActions><ArrowRight class="size-4" aria-hidden="true" /></ItemActions>
        </a>
      </Item>
    </ItemGroup>
    <Empty v-else-if="history.value.value && !history.loading.value && !history.error.value" class="border">
      <EmptyHeader>
        <EmptyTitle>{{ tr('Keine Aufgaben auf dieser Seite', 'No tasks on this page') }}</EmptyTitle>
        <EmptyDescription>{{ tr('Weitere Seiten sind über die Seitennavigation erreichbar.', 'Use the page controls to continue when more pages are available.') }}</EmptyDescription>
      </EmptyHeader>
    </Empty>
    <PageControls :count="history.value.value?.items.length ?? 0" :page="position.page" :has-previous="previous.length > 0"
      :has-next="!!history.value.value?.nextCursor && !history.error.value" :loading="history.loading.value" @next="next" @previous="back" />
  </section>
</template>
