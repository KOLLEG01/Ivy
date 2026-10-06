<script setup lang="ts">
import { CircleAlert, FileText, LoaderCircle, X } from "@lucide/vue";
import type { ComposerAttachment } from "../lib/composer";

// Files waiting to be sent with a message: image thumbnails or named file chips.
defineProps<{ items: ComposerAttachment[]; disabled?: boolean }>();
defineEmits<{ remove: [id: string] }>();
</script>

<template>
  <ul v-if="items.length" class="flex flex-wrap gap-2 px-1 pt-1 pb-2" aria-label="Attachments">
    <li
      v-for="item in items"
      :key="item.id"
      class="relative flex h-12 min-w-0 items-center rounded-xl border bg-muted/40 text-xs"
      :class="[
        item.previewUrl ? 'w-12' : 'max-w-56 gap-2 pr-4 pl-3',
        { 'border-destructive': item.state === 'failed' },
      ]"
      :title="item.detail || item.name"
    >
      <img
        v-if="item.previewUrl"
        :src="item.previewUrl"
        :alt="item.name"
        class="size-full rounded-xl object-cover"
      />
      <template v-else>
        <FileText class="size-4 shrink-0 text-muted-foreground" aria-hidden="true" />
        <span class="min-w-0 truncate">{{ item.name }}</span>
      </template>
      <span
        v-if="item.state === 'uploading'"
        class="absolute inset-0 grid place-items-center rounded-xl bg-background/60"
        role="status"
        :aria-label="'Uploading ' + item.name"
        ><LoaderCircle class="size-4 animate-spin" aria-hidden="true"
      /></span>
      <span
        v-else-if="item.state === 'failed'"
        class="absolute bottom-0.5 left-0.5 grid size-4 place-items-center rounded-full bg-background text-destructive"
        role="img"
        :aria-label="item.detail || 'Upload failed'"
        ><CircleAlert class="size-3.5" aria-hidden="true"
      /></span>
      <button
        type="button"
        class="absolute -top-1.5 -right-1.5 grid size-5 place-items-center rounded-full border bg-background text-muted-foreground shadow-xs hover:text-foreground disabled:opacity-50"
        :aria-label="'Remove ' + item.name"
        :disabled="disabled"
        @click="$emit('remove', item.id)"
      >
        <X class="size-3" aria-hidden="true" />
      </button>
    </li>
  </ul>
</template>
