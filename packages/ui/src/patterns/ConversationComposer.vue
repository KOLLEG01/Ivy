<script setup lang="ts">
import { ref } from "vue";

// A message composer card. With `acceptFiles`, pasted and dropped files are handed to the app.
const props = defineProps<{ acceptFiles?: boolean }>();
const emit = defineEmits<{ files: [files: File[]] }>();
const dragging = ref(false);
const paste = (event: ClipboardEvent) => {
  const files = Array.from(event.clipboardData?.files ?? []);
  if (!props.acceptFiles || !files.length) return;
  event.preventDefault();
  emit("files", files);
};
const dragOver = (event: DragEvent) => {
  if (!props.acceptFiles || !event.dataTransfer?.types.includes("Files")) return;
  event.preventDefault();
  dragging.value = true;
};
const drop = (event: DragEvent) => {
  dragging.value = false;
  const files = Array.from(event.dataTransfer?.files ?? []);
  if (!props.acceptFiles || !files.length) return;
  event.preventDefault();
  emit("files", files);
};
</script>

<template>
  <section
    class="ivy-composer rounded-[1.5rem] border bg-card p-3 shadow-sm transition-shadow"
    :class="{ 'ring-3 ring-ring/50': dragging }"
    @paste="paste"
    @dragover="dragOver"
    @dragleave="dragging = false"
    @drop="drop"
  >
    <slot />
  </section>
</template>

<style scoped>
/* Selects inside the composer are compact pills, like the other composer controls. */
.ivy-composer :deep([data-slot="select-trigger"]) {
  height: 1.75rem;
  gap: 0.25rem;
  border: 0;
  border-radius: 999px;
  background: transparent;
  box-shadow: none;
  padding-inline: 0.625rem;
  color: var(--muted-foreground);
  font-size: 0.75rem;
}
.ivy-composer :deep([data-slot="select-trigger"]:hover),
.ivy-composer :deep([data-slot="select-trigger"][data-state="open"]) {
  background: var(--muted);
  color: var(--foreground);
}
.ivy-composer :deep([data-slot="select-trigger"] svg) {
  width: 0.875rem;
  height: 0.875rem;
}
</style>
