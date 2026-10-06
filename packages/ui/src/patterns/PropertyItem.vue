<script setup lang="ts">
defineProps<{ label: string }>();
</script>
<template>
  <dt class="pt-1.5 text-muted-foreground">{{ label }}</dt>
  <dd
    class="flex min-h-8 min-w-0 flex-wrap items-center gap-1.5 [overflow-wrap:anywhere]"
  >
    <slot />
  </dd>
</template>
