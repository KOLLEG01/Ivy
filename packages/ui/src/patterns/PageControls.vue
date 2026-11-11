<script setup lang="ts">
import { ChevronLeft, ChevronRight } from "@lucide/vue";
import { Button } from "../components/button";
withDefaults(defineProps<{
  count: number;
  page: number;
  hasNext: boolean;
  hasPrevious?: boolean | undefined;
  loading?: boolean;
  previousLabel?: string;
  nextLabel?: string;
}>(), { hasPrevious: undefined, previousLabel: 'Previous', nextLabel: 'Next' });
defineEmits<{ next: []; previous: [] }>();
</script>
<template>
  <div
    class="mt-4 flex flex-wrap items-center justify-between gap-3 text-sm text-muted-foreground"
    aria-label="Pagination"
  >
    <p><slot name="summary">{{ count }} {{ count === 1 ? "item" : "items" }} · Page {{ page }}</slot></p>
    <div class="flex gap-2">
      <Button
        variant="outline"
        size="sm"
        :disabled="loading || !(hasPrevious ?? page > 1)"
        @click="$emit('previous')"
        ><ChevronLeft aria-hidden="true" />{{ previousLabel }}</Button
      ><Button
        variant="outline"
        size="sm"
        :disabled="loading || !hasNext"
        @click="$emit('next')"
        >{{ nextLabel }}<ChevronRight aria-hidden="true"
      /></Button>
    </div>
  </div>
</template>
