<script setup lang="ts" generic="T">
import { computed, nextTick, ref, watch } from 'vue';
import type { ComponentPublicInstance } from 'vue';
import { defaultRangeExtractor, useVirtualizer } from '@tanstack/vue-virtual';

const props = withDefaults(defineProps<{
  items: readonly T[];
  itemKey: (item: T) => string;
  itemId?: (item: T) => string;
  estimateSize?: number;
}>(), { estimateSize: 160 });
const viewport = ref<HTMLElement | null>(null), focusedKey = ref<string | null>(null);
const keys = computed(() => props.items.map(props.itemKey));
const virtualizer = useVirtualizer(computed(() => {
  const current = keys.value, focused = focusedKey.value;
  return {
    count: current.length,
    getScrollElement: () => viewport.value,
    getItemKey: (index: number) => current[index]!,
    estimateSize: () => props.estimateSize,
    overscan: 2,
    gap: 4,
    rangeExtractor: (range: Parameters<typeof defaultRangeExtractor>[0]) => {
      const indexes = defaultRangeExtractor(range), index = focused === null ? -1 : current.indexOf(focused);
      // Scrolling cannot unmount the control the reader is using.
      return index < 0 || indexes.includes(index) ? indexes : [...indexes, index].sort((a, b) => a - b);
    },
  };
}));
const rows = computed(() => virtualizer.value.getVirtualItems());
const height = computed(() => virtualizer.value.getTotalSize());
watch(keys, async current => {
  const retained = new Set(current);
  // Retain measurements for overlapping rows, but never keep previous pages' keys.
  for (const key of virtualizer.value.itemSizeCache.keys())
    if (!retained.has(String(key))) virtualizer.value.itemSizeCache.delete(key);
  if (focusedKey.value && !retained.has(focusedKey.value)) focusedKey.value = null;
  await nextTick();
  virtualizer.value.measureElement(null);
});
const measure = (element: Element | ComponentPublicInstance | null) =>
  virtualizer.value.measureElement(element as HTMLElement | null);
const focus = (event: FocusEvent) => {
  const row = (event.target as HTMLElement).closest<HTMLElement>('[data-index]');
  focusedKey.value = row ? keys.value[Number(row.dataset.index)] ?? null : null;
};
const blur = (event: FocusEvent) => {
  if (!viewport.value?.contains(event.relatedTarget as Node | null)) focusedKey.value = null;
};
const scrollToKey = (key: string, align: 'start' | 'center' | 'end' = 'start') => {
  const index = keys.value.indexOf(key);
  if (index >= 0) virtualizer.value.scrollToIndex(index, { align });
};
const navigate = (event: KeyboardEvent) => {
  if (event.target !== viewport.value || event.altKey || event.metaKey || event.shiftKey || !props.items.length) return;
  if (event.key !== 'Home' && event.key !== 'End') return;
  event.preventDefault();
  virtualizer.value.scrollToIndex(event.key === 'Home' ? 0 : props.items.length - 1,
    { align: event.key === 'Home' ? 'start' : 'end' });
};
defineExpose({ scrollToKey });
</script>

<template>
  <div ref="viewport" data-slot="virtual-list" class="min-h-0 min-w-0 overflow-y-auto overscroll-contain"
    style="overflow-anchor: none" tabindex="0" @focusin="focus" @focusout="blur" @keydown="navigate">
    <ol class="relative m-0 min-w-0 list-none p-0" :style="{ height: height + 'px' }">
      <li v-for="row in rows" :key="String(row.key)" :ref="measure" :data-index="row.index"
        :id="itemId?.(items[row.index]!)" :aria-posinset="row.index + 1" :aria-setsize="items.length"
        class="absolute top-0 left-0 w-full min-w-0" :style="{ transform: `translateY(${row.start}px)` }">
        <slot :item="items[row.index]!" :index="row.index" />
      </li>
    </ol>
  </div>
</template>
