<script setup lang="ts">
import ToolbarContent from "./ToolbarContent.vue";
// Page titles and actions live in the shell top bar; `inline` keeps them in nested panels.
defineProps<{
  title: string;
  description?: string;
  loading?: boolean;
  updatedAt?: string | null;
  inline?: boolean;
}>();
</script>
<template>
  <header
    v-if="inline"
    class="mb-6 flex flex-wrap items-start justify-between gap-3"
  >
    <div class="min-w-0 max-w-3xl [overflow-wrap:anywhere]">
      <h1 class="text-xl font-semibold tracking-tight">{{ title }}</h1>
      <p v-if="description" class="mt-1 text-sm text-muted-foreground">
        {{ description }}
      </p>
    </div>
    <div class="flex max-w-full flex-wrap items-center gap-2">
      <slot />
    </div>
  </header>
  <template v-else>
    <ToolbarContent>
      <h1 class="truncate text-sm font-medium" :title="title">{{ title }}</h1>
    </ToolbarContent>
    <ToolbarContent side="end">
      <slot />
    </ToolbarContent>
  </template>
</template>
