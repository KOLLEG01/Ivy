<script setup lang="ts">
import { computed, onBeforeUnmount, ref, watch } from 'vue';
import { Button, HierarchyTree, Kbd, SearchDialog, SearchResult, SidebarGroup, SidebarGroupAction, SidebarGroupContent, SidebarGroupLabel, SidebarMenu, SidebarMenuButton, SidebarMenuItem, useRemote } from '@ivy/ui';
import { House, Plus, Search, SquarePen } from '@lucide/vue';
import UiFrame from '../../../packages/ui-client/src/UiFrame.vue';
import { objectAncestors, objectTreeLoader } from '../../../packages/ui-client/src/object-tree';
import { IvyError, route, useHashRoute } from '../../../packages/ui-client/src/runtime';
import { newOperationId } from '../../../packages/sdk/src/client';
import { base, client } from './runtime';
import WikiBrowse from './WikiBrowse.vue';
import WikiPage from './WikiPage.vue';
const hash = useHashRoute(), tree = ref<InstanceType<typeof HierarchyTree>>();
const section = computed(() => hash.value.slice(2).split('?')[0] || 'home');
const query = computed(() => new URLSearchParams(hash.value.split('?')[1] ?? ''));
const target = computed(() => query.value.get('id') || query.value.get('parent'));
const pageLink = (id: string) => route('page', { id });
const load = objectTreeLoader(client, pageLink, 'wiki/page');
const ancestry = useRemote(signal => target.value ? objectAncestors(client, target.value, pageLink, signal) : Promise.resolve([]), 0, ["objects"]);
const revealIds = computed(() => (ancestry.value.value ?? []).map(item => item.id));
interface PendingReorder { objectId: string; beforeObjectId: string | null; mutationId: string }
const reorderStorageKey = 'ivy.wiki.reorder:' + base.href, pendingReorder = ref<PendingReorder | null>(null), reorderError = ref('');
try {
  const saved = JSON.parse(sessionStorage.getItem(reorderStorageKey) ?? 'null') as Partial<PendingReorder> | null;
  if (saved && typeof saved.objectId === 'string' && (saved.beforeObjectId === null || typeof saved.beforeObjectId === 'string') && typeof saved.mutationId === 'string') pendingReorder.value = saved as PendingReorder;
} catch { reorderError.value = 'The previous page order change could not be restored.'; }
const submitReorder = async (value: PendingReorder) => {
  try {
    await client.request('objects.reorder', value);
    sessionStorage.removeItem(reorderStorageKey); pendingReorder.value = null; reorderError.value = '';
    await tree.value?.refresh();
  } catch (cause) {
    if (cause instanceof IvyError && cause.outcome === 'not_executed') { sessionStorage.removeItem(reorderStorageKey); pendingReorder.value = null; }
    reorderError.value = cause instanceof Error ? cause.message : 'The page order could not be confirmed.';
    throw cause;
  }
};
const reorder = async (objectId: string, beforeObjectId: string | null) => {
  if (pendingReorder.value) throw new Error('Retry the previous page order change first.');
  const value = { objectId, beforeObjectId, mutationId: await newOperationId(client) };
  pendingReorder.value = value; sessionStorage.setItem(reorderStorageKey, JSON.stringify(value));
  await submitReorder(value);
};
// Dragging a page into or out of another page moves it with its current title and icon, then
// places it before the drop target through the retained reorder path.
const move = async (objectId: string, parentId: string | null, beforeObjectId: string | null) => {
  if (pendingReorder.value) throw new Error('Retry the previous page order change first.');
  const page = await client.request('objects.stat', { objectId });
  await client.request('objects.move', { objectId, parentId, name: page.name, icon: page.icon, mutationId: await newOperationId(client) });
  window.dispatchEvent(new Event('ivy:objects-changed'));
  if (beforeObjectId) await reorder(objectId, beforeObjectId);
};
const retryReorder = async () => { if (pendingReorder.value) try { await submitReorder(pendingReorder.value); } catch { /* The retained mutation can be retried again. */ } };
watch(target, () => { ancestry.value.value = null; void ancestry.refresh(); });
const changed = () => { void tree.value?.refresh(); void ancestry.refresh(); };
window.addEventListener('ivy:objects-changed', changed); onBeforeUnmount(() => window.removeEventListener('ivy:objects-changed', changed));
const navigation = computed(() => [{ label: 'Home', href: route('home'), active: section.value === 'home', icon: House }, { label: 'New page', href: route('new'), active: section.value === 'new', icon: SquarePen }]);
const searchOpen = ref(false), searchText = ref('');
let searchTimer: ReturnType<typeof setTimeout> | undefined;
const searchResults = useRemote(signal => searchText.value.trim() ? client.request('wiki.search', { text: searchText.value.trim(), limit: 20 }, { signal }) : Promise.resolve(null), 0, ["objects/wiki/page"]);
watch(searchText, () => { clearTimeout(searchTimer); searchTimer = setTimeout(() => void searchResults.refresh(), 200); });
watch(searchOpen, open => { if (!open) searchText.value = ''; });
onBeforeUnmount(() => clearTimeout(searchTimer));
</script>
<template>
  <UiFrame ui-name="Wiki" :base="base" :client="client" :navigation="navigation">
    <template #sidebar-actions>
      <SidebarGroup class="pb-0">
        <SidebarMenu>
          <SidebarMenuItem>
            <SidebarMenuButton @click="searchOpen = true"><Search aria-hidden="true" /><span>Search</span><Kbd class="ml-auto">Ctrl K</Kbd></SidebarMenuButton>
          </SidebarMenuItem>
        </SidebarMenu>
      </SidebarGroup>
    </template>
    <template #sidebar>
      <SidebarGroup>
        <SidebarGroupLabel>Pages</SidebarGroupLabel>
        <SidebarGroupAction as-child title="New page"><a :href="route('new')" aria-label="Add a page"><Plus aria-hidden="true" /></a></SidebarGroupAction>
        <SidebarGroupContent>
          <div v-if="pendingReorder || reorderError" class="px-2 pb-2 text-xs">
            <p v-if="reorderError" class="text-destructive" role="alert">{{ reorderError }}</p>
            <Button v-if="pendingReorder" variant="link" size="sm" class="h-auto px-0" @click="retryReorder">Retry order change</Button>
          </div>
          <HierarchyTree :live-scopes="['objects/wiki/page']" ref="tree" label="Pages" :storage-key="'ivy.wiki.tree:' + base.href" :load="load" :reorder="reorder" :move="move" :selected-id="target" :reveal-ids="revealIds" />
        </SidebarGroupContent>
      </SidebarGroup>
    </template>
    <WikiPage v-if="section === 'page' || section === 'new'" :key="hash" :query="query" :creating="section === 'new'" :ancestry="ancestry.value.value ?? []" :ancestry-error="ancestry.error.value" />
    <WikiBrowse v-else :key="hash" :query="query" />
  </UiFrame>
  <SearchDialog v-model:open="searchOpen" v-model:query="searchText" title="Search pages" label="Search pages" placeholder="Search titles and content…" :loading="searchResults.loading.value && !!searchText.trim()" :empty="!!searchText.trim() && !searchResults.loading.value && !searchResults.value.value?.items.length" empty-text="No matching pages">
    <SearchResult v-for="item in searchResults.value.value?.items ?? []" :key="item.id" :href="route('page', { id: item.id })" :title="item.name" :detail="item.path" />
    <p v-if="searchResults.error.value" role="alert" class="px-3 py-2 text-sm text-destructive">{{ searchResults.error.value }}</p>
  </SearchDialog>
</template>
