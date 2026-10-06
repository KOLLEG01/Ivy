<script setup lang="ts">
// A label/value property panel, as used by issue trackers and document properties.
defineProps<{ label?: string }>();
</script>
<template>
  <dl
    :aria-label="label"
    class="grid grid-cols-[minmax(6rem,auto)_minmax(0,1fr)] items-start gap-x-4 gap-y-3 text-sm"
  >
    <slot />
  </dl>
</template>
