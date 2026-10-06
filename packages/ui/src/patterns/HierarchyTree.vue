<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, reactive, ref, watch } from 'vue';
import { useRemote } from '../composables/remote';
import { ArrowDown, ArrowUp, ChevronRight, FileText, GripVertical, LoaderCircle } from '@lucide/vue';
import type { HierarchyLoader, HierarchyMove, HierarchyNode, HierarchyReorder } from './hierarchy';

const props = defineProps<{ label: string; storageKey: string; load: HierarchyLoader; liveScopes?: readonly string[]; selectedId?: string | null; revealIds?: string[]; selectOnly?: boolean; hrefFor?: (node: HierarchyNode) => string; reorder?: HierarchyReorder; move?: HierarchyMove }>();
const emit = defineEmits<{ activate: [node: HierarchyNode] }>();
const activate = (node: HierarchyNode, event?: Event) => { if (props.selectOnly) { event?.preventDefault(); emit('activate', node); } else if (!event) window.location.href = props.hrefFor?.(node) ?? node.href; };
interface Branch { items: HierarchyNode[]; cursor: string | null; loaded: boolean; loading: boolean; error: string | null }
interface Row { key: string; parent: string | null; depth: number; node?: HierarchyNode; branch?: Branch }
const branches = reactive(new Map<string | null, Branch>()), expanded = ref(new Set<string>()), tree = ref<HTMLElement>(), focused = ref('');
type DropZone = 'before' | 'after' | 'into';
const dragged = ref<Row | null>(null), dropTarget = ref<{ id: string; zone: DropZone } | null>(null), reordering = ref(''), reorderError = ref('');
const controller = new AbortController();
try { const saved: unknown = JSON.parse(sessionStorage.getItem(props.storageKey) ?? '[]'); if (Array.isArray(saved)) expanded.value = new Set(saved.filter((id): id is string => typeof id === 'string').slice(0, 500)); } catch { /* Expansion is a convenience, not content. */ }
watch(expanded, value => { try { sessionStorage.setItem(props.storageKey, JSON.stringify([...value].slice(-500))); } catch { /* Keep navigation usable without storage. */ } }, { deep: true });
const branch = (id: string | null) => { if (!branches.has(id)) branches.set(id, { items: [], cursor: null, loaded: false, loading: false, error: null }); return branches.get(id)!; };
const pending = new Map<string | null, Promise<void>>();
const loadBranch = (id: string | null, more = false): Promise<void> => {
  const existing = pending.get(id); if (existing) return existing;
  const request = fetchBranch(id, more); pending.set(id, request);
  return request.finally(() => pending.delete(id));
};
async function fetchBranch(id: string | null, more: boolean): Promise<void> {
  const value = branch(id); if (value.loading || (more && !value.cursor)) return;
  value.loading = true; value.error = null;
  try {
    const cursor = more ? value.cursor : null, result = await props.load(id, cursor, controller.signal);
    if (controller.signal.aborted) return;
    if (result.nextCursor && result.nextCursor === cursor) throw new Error('The next page repeated. Retry this branch.');
    value.items = [...new Map([...(more ? value.items : []), ...result.items].map(item => [item.id, item])).values()];
    value.cursor = result.nextCursor; value.loaded = true;
    // Restore only branches that are visible; unopened and known-empty branches never trigger a request.
    await Promise.all(value.items.filter(item => expanded.value.has(item.id) && item.hasChildren !== false && !branch(item.id).loaded).map(item => loadBranch(item.id)));
  } catch (cause) { if (!controller.signal.aborted) value.error = cause instanceof Error ? cause.message : 'Could not load this branch.'; }
  finally { value.loading = false; }
}
const rows = computed(() => {
  const result: Row[] = [], visited = new Set<string>();
  const visit = (parent: string | null, depth: number) => {
    if (depth > 128) return;
    const value = branch(parent);
    for (const node of value.items) {
      if (visited.has(node.id)) continue; visited.add(node.id);
      result.push({ key: node.id, parent, depth, node });
      if (expanded.value.has(node.id)) visit(node.id, depth + 1);
    }
    if (value.loading || value.error || value.cursor || (!value.items.length && value.loaded)) result.push({ key: 'state:' + parent, parent, depth, branch: value });
  };
  visit(null, 1); return result;
});
const leaves = computed(() => new Set([...branches.values()].flatMap(value => value.items.filter(item => item.hasChildren === false).map(item => item.id))));
const expandable = (id: string) => {
  const value = branches.get(id);
  if (value?.loaded) return !!value.items.length || !!value.cursor || !!value.error;
  return !!value?.error || !!value?.loading || !leaves.value.has(id);
};
const tabStop = computed(() => rows.value.find(row => row.node && row.key === focused.value)?.key ?? rows.value.find(row => row.node && row.key === props.selectedId)?.key ?? rows.value.find(row => row.node)?.key);
const toggle = async (id: string) => { if (expanded.value.has(id)) expanded.value.delete(id); else { expanded.value.add(id); if (!branch(id).loaded || branch(id).error) await loadBranch(id); } };
const canShift = (row: Row, direction: -1 | 1) => {
  const value = branch(row.parent), index = value.items.findIndex(item => item.id === row.key);
  return direction < 0 ? index > 0 : index >= 0 && (index < value.items.length - 1 || !!value.cursor);
};
const reorderBefore = async (row: Row, beforeId: string | null) => {
  if (!props.reorder || reordering.value) return;
  reordering.value = row.key; reorderError.value = '';
  try { await props.reorder(row.key, beforeId); await loadBranch(row.parent); }
  catch (cause) { reorderError.value = cause instanceof Error ? cause.message : 'The order could not be saved.'; }
  finally { reordering.value = ''; }
};
const shift = async (row: Row, direction: -1 | 1) => {
  let value = branch(row.parent), index = value.items.findIndex(item => item.id === row.key);
  if (direction > 0 && index === value.items.length - 1 && value.cursor) { await loadBranch(row.parent, true); value = branch(row.parent); index = value.items.findIndex(item => item.id === row.key); }
  if (index < 0) return;
  const beforeId = (direction < 0 ? value.items[index - 1]?.id : value.items[index + 2]?.id) ?? null;
  if (direction < 0 && !beforeId) return;
  await reorderBefore(row, beforeId);
};
const startDrag = (event: DragEvent, row: Row) => {
  if (!props.reorder || !row.node || reordering.value) { event.preventDefault(); return; }
  dragged.value = row; event.dataTransfer?.setData('text/plain', row.key);
  if (event.dataTransfer) event.dataTransfer.effectAllowed = 'move';
};
// With `move`, a drop above or below a row places the page beside it under that row's parent, and a
// drop on the middle of a row nests it there. Without `move`, drops only reorder siblings.
const within = (id: string | null, ancestor: string) => {
  const parents = new Map(rows.value.filter(row => row.node).map(row => [row.key, row.parent]));
  for (let current = id; current; current = parents.get(current) ?? null) if (current === ancestor) return true;
  return false;
};
const zoneOf = (event: DragEvent, row: Row): DropZone | null => {
  const source = dragged.value;
  if (!source || source.key === row.key || within(row.key, source.key)) return null;
  const bounds = (event.currentTarget as HTMLElement).getBoundingClientRect(), y = (event.clientY - bounds.top) / bounds.height;
  if (!props.move) return source.parent === row.parent ? (y < 0.5 ? 'before' : 'after') : null;
  const zone: DropZone = y < 0.25 ? 'before' : y > 0.75 ? 'after' : 'into';
  return zone === 'into' && source.parent === row.key ? null : zone;
};
const dragOver = (event: DragEvent, row: Row) => {
  const zone = zoneOf(event, row);
  if (!zone) { if (dropTarget.value?.id === row.key) dropTarget.value = null; return; }
  event.preventDefault();
  dropTarget.value = { id: row.key, zone };
  if (event.dataTransfer) event.dataTransfer.dropEffect = 'move';
};
const moveTo = async (row: Row, parentId: string | null, beforeId: string | null) => {
  if (!props.move || reordering.value) return;
  reordering.value = row.key; reorderError.value = '';
  try {
    await props.move(row.key, parentId, beforeId);
    if (parentId) expanded.value.add(parentId);
    await Promise.all([loadBranch(row.parent), loadBranch(parentId)]);
  } catch (cause) { reorderError.value = cause instanceof Error ? cause.message : 'The page could not be moved.'; }
  finally { reordering.value = ''; }
};
const drop = async (event: DragEvent, row: Row) => {
  const source = dragged.value, zone = dropTarget.value?.id === row.key ? dropTarget.value.zone : null;
  dragged.value = null; dropTarget.value = null;
  if (!source || !zone) return;
  event.preventDefault();
  if (zone === 'into') { await moveTo(source, row.key, null); return; }
  let value = branch(row.parent), index = value.items.findIndex(item => item.id === row.key);
  if (zone === 'after' && index === value.items.length - 1 && value.cursor) { await loadBranch(row.parent, true); value = branch(row.parent); index = value.items.findIndex(item => item.id === row.key); }
  const beforeId = zone === 'after' ? value.items[index + 1]?.id ?? null : row.key;
  if (beforeId === source.key) return;
  if (source.parent === row.parent) await reorderBefore(source, beforeId);
  else await moveTo(source, row.parent, beforeId);
};
const endDrag = () => { dragged.value = null; dropTarget.value = null; };
const focus = async (id: string) => { focused.value = id; await nextTick(); [...(tree.value?.querySelectorAll<HTMLElement>('[role="treeitem"]') ?? [])].find(item => item.dataset.id === id)?.focus(); };
const revealSelection = async () => {
  await nextTick();
  const element = [...(tree.value?.querySelectorAll<HTMLElement>('[role="treeitem"]') ?? [])].find(item => item.dataset.id === props.selectedId);
  if (!element || !element.getClientRects().length) return;
  // Scroll the tree's own panel without moving the document or stealing input focus.
  let panel = tree.value?.parentElement;
  while (panel && panel !== document.body && !['auto', 'scroll'].includes(getComputedStyle(panel).overflowY)) panel = panel.parentElement;
  if (!panel || panel === document.body) return;
  const item = element.getBoundingClientRect(), view = panel.getBoundingClientRect();
  if (item.top < view.top) panel.scrollTop -= view.top - item.top;
  else if (item.bottom > view.bottom) panel.scrollTop += item.bottom - view.bottom;
};
let prefix = '', prefixAt = 0;
const keydown = async (event: KeyboardEvent, row: Row) => {
  if (!row.node || event.altKey || event.ctrlKey || event.metaKey) return;
  const nodes = rows.value.filter(item => item.node), index = nodes.findIndex(item => item.key === row.key);
  const target = event.key === 'ArrowDown' ? nodes[index + 1] : event.key === 'ArrowUp' ? nodes[index - 1] : event.key === 'Home' ? nodes[0] : event.key === 'End' ? nodes.at(-1) : undefined;
  if (['ArrowDown', 'ArrowUp', 'Home', 'End'].includes(event.key)) { event.preventDefault(); if (target) await focus(target.key); }
  else if (event.key === 'ArrowRight') { event.preventDefault(); if (!expanded.value.has(row.key) && expandable(row.key)) await toggle(row.key); else { if (branch(row.key).loading) await loadBranch(row.key); const child = branch(row.key).items[0]; if (child) await focus(child.id); } }
  else if (event.key === 'ArrowLeft') { event.preventDefault(); if (expanded.value.has(row.key)) expanded.value.delete(row.key); else if (row.parent) await focus(row.parent); }
  else if (event.key === 'Enter') { event.preventDefault(); activate(row.node); }
  else if (event.key === ' ') { event.preventDefault(); if (expandable(row.key)) await toggle(row.key); }
  else if (event.key.length === 1) {
    prefix = Date.now() - prefixAt < 600 ? prefix + event.key.toLocaleLowerCase() : event.key.toLocaleLowerCase(); prefixAt = Date.now();
    const found = [...nodes.slice(index + 1), ...nodes.slice(0, index + 1)].find(item => item.node!.label.toLocaleLowerCase().startsWith(prefix));
    if (found) { event.preventDefault(); await focus(found.key); }
  }
};
let revealSerial = 0;
const reveal = async () => {
  const serial = ++revealSerial, ids = props.revealIds ?? []; let parent: string | null = null;
  if (!branch(null).loaded) await loadBranch(null);
  for (const id of ids) {
    if (serial !== revealSerial || controller.signal.aborted) return;
    const value = branch(parent);
    while (!value.items.some(item => item.id === id) && value.cursor && !value.error && !value.loading) { await loadBranch(parent, true); if (serial !== revealSerial || controller.signal.aborted) return; }
    if (!value.items.some(item => item.id === id)) break;
    if (id !== ids.at(-1)) { expanded.value.add(id); if (!branch(id).loaded) await loadBranch(id); }
    parent = id;
  }
  if (serial === revealSerial) await revealSelection();
};
watch(() => props.revealIds, reveal);
watch(() => props.selectedId, revealSelection);
const { refresh } = useRemote(async () => {
  // Hidden branches stay lazy, but their cached children may have changed too.
  const visible = new Set(rows.value.filter(row => row.node && expanded.value.has(row.key)).map(row => row.key));
  for (const [id, value] of branches) if (id !== null && !visible.has(id)) value.loaded = false;
  const refreshBranch = async (id: string | null) => {
    const count = branch(id).items.length;
    await loadBranch(id);
    while (branch(id).cursor && !branch(id).error && branch(id).items.length < count && !controller.signal.aborted) await loadBranch(id, true);
  };
  await refreshBranch(null);
  for (const row of rows.value) {
    if (row.node && expanded.value.has(row.key) && branches.has(row.key)) await refreshBranch(row.key);
  }
  await reveal();
  // Branches keep their own error UI; visible failures also need the shared retry.
  const error = rows.value.find(row => row.branch?.error)?.branch?.error;
  if (error) throw new Error(error);
}, 0, props.liveScopes);
onBeforeUnmount(() => { revealSerial++; controller.abort(); });
defineExpose({ refresh });
</script>
<template>
  <div ref="tree" role="tree" :aria-label="label" class="hierarchy-tree min-w-0 text-sm text-sidebar-foreground">
    <p v-if="reorderError" role="alert" class="mb-2 px-2 text-xs text-destructive">{{ reorderError }}</p>
    <template v-for="row in rows" :key="row.key">
      <div v-if="row.node" role="treeitem" :data-id="row.key" :aria-level="row.depth" :aria-label="row.node.label" :aria-selected="row.key === selectedId" :aria-expanded="expandable(row.key) ? expanded.has(row.key) : undefined" :tabindex="row.key === tabStop ? 0 : -1" :draggable="!!reorder" class="group relative flex h-8 min-w-0 items-center gap-1 rounded-md pr-1 outline-hidden hover:bg-sidebar-accent hover:text-sidebar-accent-foreground focus-visible:ring-2 focus-visible:ring-sidebar-ring" :class="[row.key === selectedId ? 'is-selected bg-sidebar-accent font-medium text-sidebar-accent-foreground' : 'text-sidebar-foreground/80', dropTarget?.id === row.key ? { before: 'before:absolute before:inset-x-1 before:top-0 before:border-t-2 before:border-primary', after: 'after:absolute after:inset-x-1 after:bottom-0 after:border-b-2 after:border-primary', into: 'bg-primary/10 ring-2 ring-primary/60' }[dropTarget.zone] : '']" :style="{ paddingLeft: Math.min(row.depth - 1, 8) * 12 + 4 + 'px' }" @focus="focused = row.key" @keydown="keydown($event, row)" @dragstart="startDrag($event, row)" @dragover="dragOver($event, row)" @drop="drop($event, row)" @dragend="endDrag">
        <button v-if="expandable(row.key)" tabindex="-1" class="flex size-6 shrink-0 items-center justify-center rounded-sm text-muted-foreground hover:bg-foreground/10" :aria-label="(expanded.has(row.key) ? 'Collapse ' : 'Expand ') + row.node.label" @click.stop="toggle(row.key)"><ChevronRight class="size-3.5 transition-transform" :class="{ 'rotate-90': expanded.has(row.key) }" aria-hidden="true" /></button><span v-else class="size-6 shrink-0" aria-hidden="true" />
        <span class="flex size-5 shrink-0 items-center justify-center" aria-hidden="true"><span v-if="row.node.icon" class="leading-none">{{ row.node.icon }}</span><FileText v-else class="size-4 text-muted-foreground" /></span>
        <a :href="hrefFor?.(row.node) ?? row.node.href" tabindex="-1" class="min-w-0 flex-1 truncate py-1.5 outline-hidden" :title="row.node.label" :aria-current="row.key === selectedId ? 'page' : undefined" @click="activate(row.node, $event)">{{ row.node.label }}</a>
        <div v-if="reorder" class="tree-order-actions ml-1 flex shrink-0 items-center" @mousedown.stop @click.stop @keydown.stop><GripVertical class="tree-drag-hint size-3.5 text-muted-foreground" aria-hidden="true" /><button class="flex size-6 items-center justify-center rounded-sm text-muted-foreground hover:bg-foreground/10" :aria-label="'Move ' + row.node.label + ' up'" :disabled="!!reordering || !canShift(row, -1)" @click="shift(row, -1)"><ArrowUp class="size-3.5" aria-hidden="true" /></button><button class="flex size-6 items-center justify-center rounded-sm text-muted-foreground hover:bg-foreground/10" :aria-label="'Move ' + row.node.label + ' down'" :disabled="!!reordering || !canShift(row, 1)" @click="shift(row, 1)"><ArrowDown class="size-3.5" aria-hidden="true" /></button></div>
      </div>
      <div v-else-if="row.branch" class="px-2 py-1 text-xs text-muted-foreground" :style="{ marginLeft: Math.min(row.depth - 1, 8) * 12 + 'px' }" role="none">
        <span v-if="row.branch.loading" role="status" class="flex items-center gap-2 p-2"><LoaderCircle class="size-3 animate-spin" aria-hidden="true" />Loading…</span>
        <template v-else-if="row.branch.error"><p role="alert" class="break-words">{{ row.branch.error }}</p><button class="min-h-8 underline" @click="loadBranch(row.parent, !!row.branch.cursor)">Retry branch</button></template>
        <button v-else-if="row.branch.cursor" class="min-h-8 px-2 underline" :aria-label="row.parent ? 'Load more children' : 'Load more roots'" @click="loadBranch(row.parent, true)">Load more</button>
        <p v-else-if="!row.branch.items.length" class="p-2">{{ row.parent ? 'No children' : 'Nothing here yet' }}</p>
      </div>
    </template>
  </div>
</template>
<style scoped>
.tree-order-actions { opacity: 0; }
[role="treeitem"]:hover .tree-order-actions, [role="treeitem"]:focus-within .tree-order-actions { opacity: 1; }
.tree-order-actions button:disabled { opacity: .3; }
@media (max-width: 767px), (pointer: coarse) {
  [role="treeitem"] { height: 2.5rem; }
  .tree-order-actions { display: none; }
  [role="treeitem"].is-selected .tree-order-actions { display: flex; opacity: 1; }
  .tree-drag-hint { display: none; }
}
</style>
