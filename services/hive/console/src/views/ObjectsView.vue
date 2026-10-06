<script setup lang="ts">
import { computed, nextTick, ref, watch } from 'vue';
import { FolderOpen, ListTree, Search, SlidersHorizontal, X } from '@lucide/vue';
import { BreadcrumbTrail, Button, Checkbox, HierarchyTree, Input, Label, PageControls, PageHeader, RemoteState, StatusBadge, useRemote } from '@ivy/ui';
import { objectAncestors, objectTreeLoader } from '../../../../../packages/ui-client/src/object-tree';
import { consoleBase, consoleClient, usePage } from '../runtime';
import type { Operation } from '../runtime';
import ObjectInspector from './ObjectInspector.vue';
const props = defineProps<{ query: URLSearchParams }>();
const search = ref(props.query.get('q') ?? ''), contract = ref(props.query.get('contract') ?? ''), version = ref(props.query.get('version') ?? ''), archived = ref(props.query.get('archived') === 'yes');
const inspector = ref<InstanceType<typeof ObjectInspector>>(), inspectedId = ref('');
const lookup = ref(''), filtersOpen = ref(false), lookupOpen = ref(false), treeOpen = ref(false), tree = ref<InstanceType<typeof HierarchyTree>>();
const parentId = computed(() => props.query.get('parent')), selected = computed(() => props.query.get('id') || props.query.get('path'));
const link = (values: Record<string, string> = {}) => { const query = new URLSearchParams(props.query); for (const [key, value] of Object.entries(values)) { if (value) query.set(key, value); else query.delete(key); } return '#/system/objects' + (query.size ? '?' + query.toString() : ''); };
const browseLink = (id: string) => link({ parent: id, id: '', path: '', revision: '', tab: '', q: '' });
const loadTree = objectTreeLoader(consoleClient, browseLink);
const ancestry = useRemote(signal => parentId.value ? objectAncestors(consoleClient, parentId.value, browseLink, signal) : Promise.resolve([]), 0, ["objects"]);
const revealIds = computed(() => (ancestry.value.value ?? []).map(item => item.id));
interface Row { id: string; name: string; path: string; contractKey: string; contractVersion: string; currentRevision: number; archived: boolean }
const row = (object: Operation.ObjectMetadata): Row => ({ id: object.id, name: object.name, path: object.path, contractKey: object.contractKey, contractVersion: object.contractVersion, currentRevision: object.currentRevision, archived: object.effectivelyArchived });
const listing = usePage(async (signal, cursor): Promise<{ items: Row[]; nextCursor: string | null }> => {
  const page = { limit: 50, ...(cursor ? { cursor } : {}), includeArchived: props.query.get('archived') === 'yes' };
  if (props.query.get('q')) {
    const result = await consoleClient.request('objects.search', { ...page, text: props.query.get('q')!, ...(props.query.get('contract') ? { contractKey: props.query.get('contract')! } : {}), ...(parentId.value ? { rootId: parentId.value } : {}) }, { signal });
    return { ...result, items: result.items.map(row) };
  }
  if (props.query.get('contract')) {
    const result = await consoleClient.request('objects.query', { ...page, contractKey: props.query.get('contract')!, ...(props.query.get('version') ? { contractVersions: [props.query.get('version')!] } : {}),
      ...(parentId.value ? { where: { op: 'eq' as const, field: 'object.parentId', value: parentId.value } } : {}),
      select: ['object.name', 'object.path', 'object.contractKey', 'object.contractVersion', 'object.revision', 'object.effectivelyArchived'], orderBy: [{ field: 'object.path', direction: 'asc' }] }, { signal });
    return { ...result, items: result.items.map(item => ({ id: item.objectId, name: String(item.values['object.name']), path: String(item.values['object.path']), contractKey: String(item.values['object.contractKey']),
      contractVersion: item.contractVersion, currentRevision: item.revision, archived: item.values['object.effectivelyArchived'] === true })) };
  }
  const result = await consoleClient.request('objects.list', { ...page, parentId: parentId.value ?? null }, { signal });
  return { ...result, items: result.items.map(row) };
}, () => 'ivy.objects.page:' + consoleBase.href + JSON.stringify(['parent', 'q', 'contract', 'version', 'archived'].map(key => props.query.get(key))), ["objects"]);

const listingKey = computed(() => JSON.stringify(['parent', 'q', 'contract', 'version', 'archived'].map(key => props.query.get(key))));
watch(listingKey, () => { search.value = props.query.get('q') ?? ''; contract.value = props.query.get('contract') ?? ''; version.value = props.query.get('version') ?? ''; archived.value = props.query.get('archived') === 'yes'; listing.restore(); treeOpen.value = false; });
watch(parentId, () => { ancestry.value.value = null; void ancestry.refresh(); });
const applyFilters = () => { window.location.hash = link({ q: search.value, contract: contract.value, version: search.value ? '' : version.value, archived: archived.value ? 'yes' : '', id: '', path: '', revision: '' }); filtersOpen.value = false; };
const openLookup = () => { window.location.hash = link({ id: lookup.value.startsWith('/') ? '' : lookup.value.trim(), path: lookup.value.startsWith('/') ? lookup.value.trim() : '', revision: '', tab: '' }); lookupOpen.value = false; };
interface OpenObject { id: string; name: string; href: string }
const tabs = ref<OpenObject[]>([]), tabsKey = 'ivy.objects.tabs:' + consoleBase.href;
try { const saved: unknown = JSON.parse(sessionStorage.getItem(tabsKey) ?? '[]'); if (Array.isArray(saved)) tabs.value = saved.filter((item): item is OpenObject => typeof item?.id === 'string' && typeof item?.name === 'string' && typeof item?.href === 'string' && item.href.startsWith('#/system/objects?')).slice(-8); } catch { /* Browsing works without storage. */ }
watch(tabs, value => { try { sessionStorage.setItem(tabsKey, JSON.stringify(value)); } catch { /* Optional view state. */ } }, { deep: true });
const opened = (item: { id: string; name: string; parentId: string | null }) => { inspectedId.value = item.id; if (item.parentId && !props.query.has('parent') && !props.query.get('q') && !props.query.get('contract')) window.location.hash = link({ parent: item.parentId }); const tab = { ...item, href: link({ id: item.id, path: '' }) }; const index = tabs.value.findIndex(value => value.id === item.id); if (index < 0) tabs.value = [...tabs.value, tab].slice(-8); else tabs.value[index] = tab; };
const closeTab = (id: string) => { const index = tabs.value.findIndex(item => item.id === id); tabs.value = tabs.value.filter(item => item.id !== id); if (inspectedId.value === id) window.location.hash = tabs.value[Math.max(0, index - 1)]?.href ?? link({ id: '', path: '', revision: '', tab: '' }); };
const refresh = () => { inspector.value?.refresh(); void listing.refresh(); void ancestry.refresh(); void tree.value?.refresh(); };
const listPanel = ref<HTMLElement>();
watch(selected, async (value, previous) => { inspectedId.value = props.query.get('id') ?? '';  if (previous && !value) { await nextTick(); listPanel.value?.focus({ preventScroll: true }); } });
</script>
<template>
  <PageHeader title="Object Browser" :loading="listing.loading.value" :updated-at="listing.updatedAt.value"><Button variant="ghost" size="sm" @click="lookupOpen = !lookupOpen">Open Object</Button></PageHeader>
  <form v-if="lookupOpen" class="mb-4 flex flex-wrap items-end gap-2" @submit.prevent="openLookup"><div class="min-w-0 flex-1"><Label for="object-lookup">Object ID or absolute path</Label><Input id="object-lookup" v-model="lookup" class="mt-2" placeholder="Object ID or /path" required /></div><Button type="submit">Open</Button></form>
  <div v-if="tabs.length" aria-label="Open objects" class="mb-4 flex overflow-x-auto border-b"><div v-for="tab in tabs" :key="tab.id" class="flex shrink-0 items-center border-b-2" :class="inspectedId === tab.id ? 'border-foreground bg-muted' : 'border-transparent'"><a :href="tab.href" class="max-w-48 truncate px-3 py-3 text-xs">{{ tab.name }}</a><Button variant="ghost" size="icon" class="size-11 rounded-none" :aria-label="'Close ' + tab.name" @click="closeTab(tab.id)"><X class="size-3" aria-hidden="true" /></Button></div></div>
  <div class="object-workbench" :class="{ 'has-selection': selected }">
    <aside class="object-tree" :class="{ 'show-tree': treeOpen }" aria-label="Object hierarchy"><div class="mb-2 flex items-center justify-between"><a :href="browseLink('')" class="px-2 py-3 text-sm font-medium">All objects</a><Button variant="ghost" size="icon" class="xl:hidden" aria-label="Close hierarchy" @click="treeOpen = false"><X aria-hidden="true" /></Button></div><HierarchyTree :live-scopes="['objects']" ref="tree" label="Object tree" :storage-key="'ivy.objects.tree:' + consoleBase.href" :load="loadTree" :href-for="node => browseLink(node.id)" :selected-id="parentId" :reveal-ids="revealIds" /></aside>
    <section ref="listPanel" tabindex="-1" class="object-list min-w-0 outline-none" aria-label="Object list">
      <BreadcrumbTrail label="Object breadcrumbs" :root="{ label: 'Root', href: browseLink('') }" :items="ancestry.value.value ?? []" /><p v-if="ancestry.error.value" role="alert" class="mb-3 text-xs">{{ ancestry.error.value }}</p>
      <form class="mb-3 flex items-center gap-2" @submit.prevent="applyFilters"><Button variant="ghost" size="icon" class="xl:hidden" aria-label="Show hierarchy" :aria-expanded="treeOpen" @click="treeOpen = !treeOpen"><ListTree aria-hidden="true" /></Button><Input v-model="search" aria-label="Search names and text" placeholder="Search objects…" class="min-w-0" /><Button variant="ghost" size="icon" type="submit" aria-label="Search objects"><Search aria-hidden="true" /></Button><Button variant="ghost" size="icon" aria-label="Object filters" :aria-expanded="filtersOpen" @click="filtersOpen = !filtersOpen"><SlidersHorizontal aria-hidden="true" /></Button></form>
      <form v-if="filtersOpen" class="mb-4 grid gap-3 rounded-lg border bg-muted/30 p-3" @submit.prevent="applyFilters"><div><Label for="object-contract">Exact contract key</Label><Input id="object-contract" v-model="contract" class="mt-1" placeholder="All contracts" /></div><div><Label for="object-version">Contract version</Label><Input id="object-version" v-model="version" class="mt-1" :disabled="!contract || !!search" placeholder="All versions" /></div><Label class="flex min-h-9 items-center gap-2"><Checkbox v-model="archived" />Include archived</Label><Button variant="outline" type="submit">Apply filters</Button></form>
      <div v-if="query.get('contract') || query.get('archived')" class="mb-3 flex flex-wrap items-center gap-2 text-xs text-muted-foreground"><span v-if="query.get('contract')">{{ query.get('contract') }}{{ query.get('version') ? ' · ' + query.get('version') : '' }}</span><span v-if="query.get('archived')">Including archived</span><a :href="link({ contract: '', version: '', archived: '' })" class="underline">Clear filters</a></div>
      <RemoteState :loading="listing.loading.value" :error="listing.error.value" :has-data="!!listing.value.value" :empty="listing.value.value?.items.length === 0" empty-title="No objects found" empty-detail="Choose another branch or change your search." @retry="listing.refresh" />
      <Button v-if="listing.error.value && listing.page.value > 1" variant="ghost" @click="listing.reset">Restart from first page</Button><div class="divide-y" role="list"><div v-for="item in listing.value.value?.items ?? []" :key="item.id" role="listitem" class="flex min-w-0 items-center gap-2 rounded-md px-2 hover:bg-muted" :class="query.get('id') === item.id ? 'bg-accent' : ''"><a :href="link({ id: item.id, path: '', revision: '', tab: 'content' })" :aria-current="query.get('id') === item.id ? 'page' : undefined" class="min-w-0 flex-1 py-3"><span class="block truncate text-sm font-medium">{{ item.name }}</span><span class="mt-1 block truncate text-xs text-muted-foreground">{{ item.contractKey }} · {{ item.contractVersion }}</span><StatusBadge v-if="item.archived" class="mt-1" label="Archived" /></a><Button variant="ghost" size="icon" :aria-label="'Browse children of ' + item.name" as-child><a :href="browseLink(item.id)"><FolderOpen aria-hidden="true" /></a></Button></div></div>
      <PageControls v-if="listing.value.value && (listing.value.value.nextCursor || listing.page.value > 1)" :count="listing.value.value.items.length" :page="listing.page.value" :has-next="!!listing.value.value.nextCursor" :loading="listing.loading.value" @next="listing.next" @previous="listing.previous" />
    </section>
    <ObjectInspector v-if="selected" ref="inspector" :key="selected + ':' + (query.get('revision') ?? '')" class="object-inspector" :query="query" :link="link" @opened="opened" />
    <div v-else class="object-placeholder hidden items-center justify-center text-sm text-muted-foreground xl:flex">Select an object to explore its content</div>
  </div>
</template>
<style scoped>
.object-workbench { display: grid; min-width: 0; gap: 1.25rem; }
.object-tree { display: none; min-width: 0; }
.object-tree.show-tree { display: block; max-height: 60dvh; overflow-y: auto; border-bottom: 1px solid var(--border); padding-bottom: 1rem; }
.has-selection .object-list, .has-selection .object-tree { display: none; }
@media (min-width: 1280px) {
  .object-workbench { grid-template-columns: 180px minmax(240px, .85fr) minmax(300px, 1.15fr); gap: 0; min-height: calc(100dvh - 200px); }
  .object-tree, .has-selection .object-tree { display: block; padding-right: .75rem; max-height: calc(100dvh - 210px); overflow-y: auto; }
  .object-list, .has-selection .object-list { display: block; padding-inline: 1rem; border-inline: 1px solid var(--border); }
  .object-inspector { padding-left: 1.25rem; }
}
</style>
