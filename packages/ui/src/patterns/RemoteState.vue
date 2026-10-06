<script setup lang="ts">
import { Inbox, LoaderCircle, TriangleAlert } from "@lucide/vue";
import { Alert, AlertDescription, AlertTitle } from "../components/alert";
import { Button } from "../components/button";
import {
  Empty,
  EmptyDescription,
  EmptyHeader,
  EmptyMedia,
  EmptyTitle,
} from "../components/empty";
defineProps<{
  loading: boolean;
  error: string | null;
  empty?: boolean;
  hasData?: boolean;
  emptyTitle?: string;
  emptyDetail?: string;
}>();
defineEmits<{ retry: [] }>();
</script>
<template>
  <Alert v-if="error" variant="destructive" class="mb-4" role="alert">
    <TriangleAlert aria-hidden="true" />
    <AlertTitle>{{
      hasData
        ? "Showing the last successful response"
        : "This view is unavailable"
    }}</AlertTitle>
    <AlertDescription class="flex flex-wrap items-center justify-between gap-3">
      <span class="min-w-0 break-words">{{ error }}</span>
      <Button
        type="button"
        size="sm"
        variant="outline"
        :disabled="loading"
        @click="$emit('retry')"
        >Retry</Button
      >
    </AlertDescription>
  </Alert>
  <div
    v-if="loading && !hasData"
    class="flex min-h-40 items-center justify-center gap-2 text-sm text-muted-foreground"
    role="status"
  >
    <LoaderCircle class="size-4 animate-spin" aria-hidden="true" />Loading…
  </div>
  <Empty v-else-if="empty && !error" class="min-h-48">
    <EmptyHeader>
      <EmptyMedia variant="icon"><Inbox aria-hidden="true" /></EmptyMedia>
      <EmptyTitle>{{ emptyTitle ?? "Nothing here yet" }}</EmptyTitle>
      <EmptyDescription v-if="emptyDetail">{{ emptyDetail }}</EmptyDescription>
    </EmptyHeader>
    <slot name="empty" />
  </Empty>
</template>
