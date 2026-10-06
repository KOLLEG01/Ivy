<script setup lang="ts">
import { ref } from 'vue';
import { ChevronRight, FileText, ListFilter, Plus, Search } from '@lucide/vue';
import { Button, DropdownMenu, DropdownMenuCheckboxItem, DropdownMenuContent, DropdownMenuTrigger, Input, PageControls, RemoteState, StatusBadge, ToolbarContent, useRemote } from '@ivy/ui';
import { route, usePage } from '../../../packages/ui-client/src/runtime';
import { client } from './runtime';
const props = defineProps<{ query: URLSearchParams }>();
const parentId = props.query.get('parent'), text = ref(props.query.get('q') ?? ''), archived = ref(props.query.get('archived') === 'yes');
const parent = useRemote(signal => parentId ? client.request('objects.stat', { objectId: parentId }, { signal }) : Promise.resolve(null), 0, ["objects"]);
const pages = usePage(async (signal, cursor) => {
  const page = { limit: 50, ...(cursor ? { cursor } : {}), includeArchived: props.query.get('archived') === 'yes' };
  if (props.query.get('q')) {
    const result = await client.request('wiki.search', { ...page, text: props.query.get('q')!, ...(parentId ? { rootId: parentId } : {}) }, { signal });
    return { ...result, items: result.items.map(item => ({ id: item.id, name: item.name, path: item.path, archived: item.effectivelyArchived, version: item.contractVersion })) };
  }
  const result = await client.request('wiki.list', { ...page, parentId }, { signal });
  return { ...result, items: result.items.map(item => ({ id: item.objectId, name: String(item.values['object.name']), path: String(item.values['object.path']), archived: item.values['object.effectivelyArchived'] === true, version: item.contractVersion })) };
}, 0, undefined, ["objects"]);
const search = () => { location.hash = route('home', { q: text.value, parent: parentId ?? '', archived: archived.value ? 'yes' : '' }); };
const toggleArchived = (value: boolean) => { archived.value = value; search(); };
</script>
<template>
  <ToolbarContent side="end"><Button size="sm" as-child><a :href="route('new', { parent: parentId ?? '' })" aria-label="New page"><Plus aria-hidden="true" /><span class="hidden sm:inline">New page</span></a></Button></ToolbarContent>
  <div class="mx-auto w-full max-w-3xl">
    <h1 class="mt-2 mb-6 text-3xl font-bold tracking-tight sm:mt-8">{{ parent.value.value ? parent.value.value.name : 'Wiki' }}</h1>
    <form class="mb-6 flex items-center gap-2" role="search" @submit.prevent="search">
      <div class="relative min-w-0 flex-1"><Search class="pointer-events-none absolute top-1/2 left-2.5 size-4 -translate-y-1/2 text-muted-foreground" aria-hidden="true" /><Input id="wiki-search" v-model="text" aria-label="Search pages" class="pl-8" placeholder="Search titles and Markdown" /></div>
      <DropdownMenu><DropdownMenuTrigger as-child><Button type="button" variant="outline" size="icon" aria-label="Search options" :class="{ 'bg-muted': archived }"><ListFilter aria-hidden="true" /></Button></DropdownMenuTrigger><DropdownMenuContent align="end"><DropdownMenuCheckboxItem :model-value="archived" @update:model-value="toggleArchived($event === true)">Include archived</DropdownMenuCheckboxItem></DropdownMenuContent></DropdownMenu>
    </form>
    <RemoteState :loading="pages.loading.value" :error="pages.error.value" :has-data="!!pages.value.value" :empty="pages.value.value?.items.length === 0" :empty-title="props.query.get('q') ? 'No matching pages' : 'A place for your next idea'" :empty-detail="props.query.get('q') ? 'Try another search term.' : 'Create a page to start collecting knowledge.'" @retry="pages.refresh" />
    <div v-if="pages.value.value?.items.length" class="divide-y rounded-lg border">
      <div v-for="page in pages.value.value.items" :key="page.id" class="group relative flex items-center gap-3 px-3 py-2.5 hover:bg-muted/50">
        <FileText class="size-4 shrink-0 text-muted-foreground" aria-hidden="true" />
        <a :href="route('page', { id: page.id })" class="min-w-0 flex-1 truncate text-sm font-medium after:absolute after:inset-0">{{ page.name }}</a>
        <StatusBadge v-if="page.archived" label="Archived" /><StatusBadge v-else-if="page.version !== '1.0.0'" label="Read-only version" tone="warning" />
        <Button variant="ghost" size="icon-sm" class="relative z-10" :aria-label="'Subpages of ' + page.name" as-child><a :href="route('home', { parent: page.id })"><ChevronRight aria-hidden="true" /></a></Button>
      </div>
    </div>
    <PageControls v-if="pages.value.value && (pages.value.value.nextCursor || pages.page.value > 1)" :count="pages.value.value.items.length" :page="pages.page.value" :has-next="!!pages.value.value.nextCursor" :loading="pages.loading.value" @next="pages.next" @previous="pages.previous" />
  </div>
</template>
