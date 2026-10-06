<script setup lang="ts">
import { useId } from "vue";
defineProps<{ title: string; description?: string }>();
const id = useId();
</script>
<template>
  <section
    :aria-labelledby="id"
    class="grid gap-4 border-b py-6 first:pt-0 last:border-b-0 md:grid-cols-[minmax(0,16rem)_minmax(0,1fr)] md:gap-8"
  >
    <div class="min-w-0">
      <h2 :id="id" class="text-sm font-medium">{{ title }}</h2>
      <p v-if="description" class="mt-1 text-sm text-muted-foreground">
        {{ description }}
      </p>
      <slot name="aside" />
    </div>
    <div class="min-w-0"><slot /></div>
  </section>
</template>
