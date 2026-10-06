<script setup lang="ts">
defineProps<{ href: string; title: string; detail?: string }>();
</script>
<template>
  <a
    :href="href"
    class="flex min-w-0 flex-col gap-0.5 rounded-md px-3 py-2 text-sm outline-hidden hover:bg-accent focus-visible:bg-accent"
  >
    <span class="truncate font-medium">{{ title }}</span>
    <span v-if="detail" class="truncate text-xs text-muted-foreground">{{
      detail
    }}</span>
  </a>
</template>
