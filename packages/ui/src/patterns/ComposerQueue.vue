<script setup lang="ts">
import { CornerDownRight, Paperclip, Pencil, X } from "@lucide/vue";
import { Button } from "../components/button";
import type { QueuedMessage } from "../lib/composer";

// Messages waiting for the running turn. They are sent in order when it ends, or steered into it now.
defineProps<{ items: QueuedMessage[]; canSteer?: boolean; disabled?: boolean }>();
defineEmits<{ steer: [id: string]; retry: [id: string]; edit: [id: string]; remove: [id: string] }>();
</script>

<template>
  <section v-if="items.length" aria-label="Queued messages" class="mb-2 space-y-1">
    <div
      v-for="item in items"
      :key="item.id"
      class="flex min-w-0 items-center gap-2 rounded-xl border bg-card px-3 py-1.5 text-sm shadow-xs"
      :class="{ 'border-destructive': item.error }"
    >
      <span class="shrink-0 text-xs" :class="item.error ? 'text-destructive' : 'text-muted-foreground'">{{
        item.error ? "Not sent" : item.sending ? "Sending…" : "Queued"
      }}</span>
      <span class="min-w-0 flex-1 truncate" :title="item.error || item.text">{{ item.text || "Attachments" }}</span>
      <span
        v-if="item.attachments"
        class="flex shrink-0 items-center gap-0.5 text-xs text-muted-foreground"
        :aria-label="item.attachments + (item.attachments === 1 ? ' attachment' : ' attachments')"
        ><Paperclip class="size-3" aria-hidden="true" />{{ item.attachments }}</span
      >
      <Button
        v-if="item.error"
        variant="ghost"
        size="sm"
        class="h-7 shrink-0 rounded-full px-2 text-xs"
        :disabled="disabled"
        @click="$emit('retry', item.id)"
        >Retry</Button
      >
      <Button
        v-else-if="canSteer && !item.sending"
        variant="ghost"
        size="sm"
        class="h-7 shrink-0 rounded-full px-2 text-xs"
        :disabled="disabled"
        :aria-label="'Steer with ' + (item.text || 'queued message')"
        title="Send into the running turn now"
        @click="$emit('steer', item.id)"
        ><CornerDownRight class="size-3.5" aria-hidden="true" />Steer</Button
      >
      <Button
        variant="ghost"
        size="icon-sm"
        class="size-7 shrink-0 rounded-full"
        :disabled="disabled || item.sending"
        :aria-label="'Edit queued message ' + (item.text || '')"
        @click="$emit('edit', item.id)"
        ><Pencil class="size-3.5" aria-hidden="true"
      /></Button>
      <Button
        variant="ghost"
        size="icon-sm"
        class="size-7 shrink-0 rounded-full"
        :disabled="disabled || item.sending"
        :aria-label="'Remove queued message ' + (item.text || '')"
        @click="$emit('remove', item.id)"
        ><X class="size-3.5" aria-hidden="true"
      /></Button>
    </div>
  </section>
</template>
