<script setup lang="ts">
import { computed, onBeforeUnmount, ref, watch } from 'vue';

const props = defineProps<{ target: HTMLElement | null; attentionSelector?: string; label?: string; leftAttentionLabel?: string; rightAttentionLabel?: string }>();
const maximum = ref(0), position = ref(0), visible = ref(100);
const left = ref<HTMLElement | null>(null), right = ref<HTMLElement | null>(null);
let frame = 0;
const measure = () => {
  const target = props.target;
  if (!target) return;
  maximum.value = Math.max(0, target.scrollWidth - target.clientWidth);
  position.value = target.scrollLeft;
  visible.value = Math.min(100, target.clientWidth / Math.max(1, target.scrollWidth) * 100);
  const bounds = target.getBoundingClientRect();
  left.value = null; right.value = null;
  for (const item of target.querySelectorAll<HTMLElement>(props.attentionSelector ?? '[data-attention]')) {
    if (!item.getClientRects().length) continue;
    const box = item.getBoundingClientRect();
    if (box.left < bounds.left - 1) left.value = item;
    if (box.right > bounds.right + 1 && !right.value) right.value = item;
  }
};
const schedule = () => { cancelAnimationFrame(frame); frame = requestAnimationFrame(measure); };
watch(() => props.target, (target, _previous, cleanup) => {
  if (!target) return;
  const resize = new ResizeObserver(schedule), changes = new MutationObserver(schedule);
  resize.observe(target);
  changes.observe(target, { childList: true, subtree: true, attributes: true });
  target.addEventListener('scroll', schedule, { passive: true });
  schedule();
  cleanup(() => { resize.disconnect(); changes.disconnect(); target.removeEventListener('scroll', schedule); });
}, { immediate: true });
onBeforeUnmount(() => cancelAnimationFrame(frame));
const offset = computed(() => maximum.value ? position.value / maximum.value * (100 - visible.value) : 0);
const jump = (item: HTMLElement | null) => {
  if (!item || !props.target) return;
  const bounds = props.target.getBoundingClientRect(), box = item.getBoundingClientRect();
  props.target.scrollBy({ left: box.left - bounds.left - 12, behavior: 'smooth' });
};
const move = (event: Event) => { if (props.target) props.target.scrollLeft = Number((event.target as HTMLInputElement).value); };
</script>

<template>
  <div v-if="maximum > 1" data-slot="horizontal-scroll-indicator" class="flex h-5 shrink-0 items-center gap-2">
    <button type="button" class="flex size-5 shrink-0 items-center justify-center rounded-full focus-visible:ring-2 focus-visible:ring-ring" :class="{ 'invisible': !left }" :aria-label="leftAttentionLabel ?? 'A ticket to the left needs you'" :title="leftAttentionLabel ?? 'A ticket to the left needs you'" @click="jump(left)"><span class="size-2 rounded-full bg-orange-500" /></button>
    <div class="relative h-1 flex-1 rounded-full bg-muted">
      <span class="absolute inset-y-0 rounded-full bg-muted-foreground/60" :style="{ width: visible + '%', left: offset + '%' }" />
      <input type="range" min="0" :max="maximum" :value="position" :aria-label="label ?? 'Scroll columns'" class="absolute -top-2 h-5 w-full cursor-ew-resize opacity-0" @input="move" />
    </div>
    <button type="button" class="flex size-5 shrink-0 items-center justify-center rounded-full focus-visible:ring-2 focus-visible:ring-ring" :class="{ 'invisible': !right }" :aria-label="rightAttentionLabel ?? 'A ticket to the right needs you'" :title="rightAttentionLabel ?? 'A ticket to the right needs you'" @click="jump(right)"><span class="size-2 rounded-full bg-orange-500" /></button>
  </div>
</template>
