<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref, useId, watch } from "vue";
import { Button } from "../components/button";

const props = withDefaults(
  defineProps<{
    enabled?: boolean;
    previewLines?: number;
    showMoreLabel?: string;
    showLessLabel?: string;
  }>(),
  {
    enabled: true,
    previewLines: 5,
    showMoreLabel: "Show more",
    showLessLabel: "Show less",
  },
);
const contentId = useId();
const content = ref<HTMLDivElement | null>(null);
const expanded = ref(false),
  overflowing = ref(false),
  previewHeight = ref(100);
const collapsed = computed(
  () => props.enabled && overflowing.value && !expanded.value,
);
let observer: ResizeObserver | undefined;
const measure = () => {
  if (!content.value) return;
  const style = getComputedStyle(content.value);
  const lineHeight =
    parseFloat(style.lineHeight) || parseFloat(style.fontSize) * 1.5;
  previewHeight.value = lineHeight * props.previewLines;
  overflowing.value =
    content.value.getBoundingClientRect().height > previewHeight.value + 1;
};
onMounted(() => {
  observer = new ResizeObserver(measure);
  observer.observe(content.value!);
  measure();
});
watch(() => [props.enabled, props.previewLines], measure, { flush: "post" });
onBeforeUnmount(() => observer?.disconnect());
</script>

<template>
  <div data-slot="expandable-content" class="min-w-0">
    <div
      :id="contentId"
      data-slot="expandable-content-viewport"
      class="relative"
      :class="collapsed && 'overflow-hidden'"
      :style="collapsed ? { maxHeight: previewHeight + 'px' } : undefined"
    >
      <div ref="content" class="flow-root"><slot /></div>
      <div
        v-if="collapsed"
        data-slot="expandable-content-fade"
        aria-hidden="true"
        class="pointer-events-none absolute inset-x-0 bottom-0 h-10 bg-linear-to-t from-card to-transparent"
      />
    </div>
    <Button
      v-if="enabled && overflowing"
      variant="link"
      size="sm"
      class="h-auto px-0 py-1"
      :aria-controls="contentId"
      :aria-expanded="expanded"
      @click="expanded = !expanded"
    >
      {{ expanded ? showLessLabel : showMoreLabel }}
    </Button>
  </div>
</template>
