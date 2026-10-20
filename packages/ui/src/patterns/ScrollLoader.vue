<script setup lang="ts">
import { onBeforeUnmount, onMounted, ref, watch } from 'vue';
import { Button } from '../components/button';

const props = defineProps<{ loading: boolean; error?: string | null; label: string; enabled?: boolean }>();
const emit = defineEmits<{ load: [] }>();
const sentinel = ref<HTMLElement>(), visible = ref(false), scrolled = ref(0);
let observer: IntersectionObserver | undefined;
const scroll = () => { scrolled.value++; };
onMounted(() => {
  observer = new IntersectionObserver(entries => { visible.value = !!entries[0]?.isIntersecting; }, { rootMargin: '120px' });
  if (sentinel.value) observer.observe(sentinel.value);
  window.addEventListener('scroll', scroll, { passive: true, capture: true });
});
onBeforeUnmount(() => {
  observer?.disconnect();
  window.removeEventListener('scroll', scroll, true);
});
watch([visible, scrolled, () => props.enabled], () => {
  // Scroll anchoring can move this sentinel before IntersectionObserver catches up.
  const bounds = sentinel.value?.getBoundingClientRect();
  if (visible.value && scrolled.value && bounds && bounds.bottom >= -120 && bounds.top <= innerHeight + 120 &&
    props.enabled !== false && !props.loading && !props.error) emit('load');
});
</script>

<template>
  <div ref="sentinel" class="flex flex-col items-center gap-2 py-2" :aria-busy="loading">
    <p v-if="error" role="alert" class="text-sm text-destructive">{{ error }}</p>
    <Button variant="outline" size="sm" :disabled="loading" @click="emit('load')">{{ label }}</Button>
  </div>
</template>
