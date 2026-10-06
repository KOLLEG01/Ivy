<script setup lang="ts">
import { ChevronLeft, ChevronRight } from "@lucide/vue";
import { Button } from "../components/button";
defineProps<{
  count: number;
  page: number;
  hasNext: boolean;
  loading?: boolean;
}>();
defineEmits<{ next: []; previous: [] }>();
</script>
<template>
  <div
    class="mt-4 flex flex-wrap items-center justify-between gap-3 text-sm text-muted-foreground"
    aria-label="Pagination"
  >
    <p>{{ count }} {{ count === 1 ? "item" : "items" }} · Page {{ page }}</p>
    <div class="flex gap-2">
      <Button
        variant="outline"
        size="sm"
        :disabled="loading || page <= 1"
        @click="$emit('previous')"
        ><ChevronLeft aria-hidden="true" />Previous</Button
      ><Button
        variant="outline"
        size="sm"
        :disabled="loading || !hasNext"
        @click="$emit('next')"
        >Next<ChevronRight aria-hidden="true"
      /></Button>
    </div>
  </div>
</template>
