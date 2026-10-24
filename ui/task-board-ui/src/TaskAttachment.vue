<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref } from "vue";
import {
  Button,
  ContentView,
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
  SegmentedControl,
} from "@ivy/ui";
import { Paperclip } from "@lucide/vue";
import { attachmentView } from "./attachment-view";
import { attachmentBytes, attachmentSource, saveBytes } from "./attachment-images";
import type { TaskBoard } from "./runtime";

const props = defineProps<{
  attachment: TaskBoard.TaskAttachment;
  taskId: string;
  loadImage: (source: string) => Promise<string | null>;
  previewImage: (source: string) => void;
}>();
const kind = computed(() =>
  attachmentView(props.attachment.filename, props.attachment.mediaType),
);
const busy = ref(false),
  error = ref<string | null>(null),
  previewUrl = ref<string | null>(null),
  previewText = ref<string | null>(null),
  dialogOpen = ref(false),
  htmlMode = ref<"preview" | "source">("preview");
let cachedBytes: Uint8Array | null = null;
const bytes = async () => {
  cachedBytes ??= await attachmentBytes(props.attachment, props.taskId);
  return cachedBytes;
};
const loadPreview = async () => {
  if (
    kind.value === "download" ||
    previewUrl.value ||
    previewText.value !== null
  )
    return;
  busy.value = true;
  error.value = null;
  try {
    if (kind.value === "image") {
      previewUrl.value = await props.loadImage(attachmentSource(props.attachment.attachmentId));
      if (!previewUrl.value) throw new Error("The saved attachment is unavailable.");
      return;
    }
    const value = await bytes();
    if (kind.value === "markdown" || kind.value === "text" || kind.value === "html")
      previewText.value = new TextDecoder().decode(value);
    else
      previewUrl.value = URL.createObjectURL(
        new Blob([new Uint8Array(value)], { type: props.attachment.mediaType }),
      );
  } catch (cause) {
    error.value =
      cause instanceof Error
        ? cause.message
        : "The saved attachment is unavailable.";
  } finally {
    busy.value = false;
  }
};
const openPreview = () => {
  dialogOpen.value = true;
  void loadPreview();
};
const download = async () => {
  busy.value = true;
  error.value = null;
  try {
    saveBytes(await bytes(), props.attachment.filename);
  } catch (cause) {
    error.value =
      cause instanceof Error
        ? cause.message
        : "The saved attachment is unavailable.";
  } finally {
    busy.value = false;
  }
};
onMounted(() => {
  if (kind.value === "image" || kind.value === "video") void loadPreview();
});
onBeforeUnmount(() => {
  if (previewUrl.value && kind.value !== "image") URL.revokeObjectURL(previewUrl.value);
});
</script>

<template>
  <div class="space-y-2 rounded-lg border p-3">
    <div class="flex flex-wrap items-center gap-2">
      <Paperclip
        class="size-4 shrink-0 text-muted-foreground"
        aria-hidden="true"
      />
      <span class="min-w-0 flex-1 break-all text-sm">
        <button
          v-if="kind === 'markdown' || kind === 'html' || kind === 'text' || kind === 'pdf'"
          type="button"
          class="text-left underline-offset-2 hover:underline"
          :disabled="busy"
          :aria-label="'Open ' + attachment.filename"
          @click="openPreview"
        >
          {{ attachment.filename }}
        </button>
        <span v-else>{{ attachment.filename }}</span>
        <span class="text-xs text-muted-foreground">
          · {{ Math.ceil(attachment.byteLength / 1024) }} KiB</span
        >
      </span>
      <Button
        v-if="kind === 'markdown' || kind === 'html' || kind === 'text' || kind === 'pdf'"
        type="button"
        variant="outline"
        size="sm"
        :disabled="busy"
        @click="openPreview"
        >Preview</Button
      >
      <Button
        type="button"
        variant="ghost"
        size="sm"
        :disabled="busy"
        @click="download"
        >Download</Button
      >
    </div>
    <p
      v-if="busy && (kind === 'image' || kind === 'video')"
      role="status"
      class="text-sm text-muted-foreground"
    >
      Loading preview…
    </p>
    <button
      v-if="previewUrl && kind === 'image'"
      type="button"
      class="block max-w-full rounded-md focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
      :aria-label="'Enlarge ' + attachment.filename"
      @click="previewImage(attachmentSource(attachment.attachmentId))"
    ><img :src="previewUrl" :alt="attachment.filename" class="max-h-80 max-w-full rounded-md object-contain" /></button>
    <video
      v-if="previewUrl && kind === 'video'"
      :src="previewUrl"
      controls
      preload="metadata"
      class="max-h-80 max-w-full rounded-md"
      :aria-label="attachment.filename"
    />
    <p v-if="error" role="alert" class="text-sm text-destructive">
      {{ error }}
    </p>
  </div>
  <Dialog v-model:open="dialogOpen">
    <DialogContent
      class="max-h-[90dvh] w-[calc(100vw-2rem)] overflow-y-auto sm:max-w-4xl"
    >
      <DialogHeader>
        <DialogTitle class="break-all">{{ attachment.filename }}</DialogTitle>
        <DialogDescription>Saved attachment preview</DialogDescription>
      </DialogHeader>
      <SegmentedControl
        v-if="kind === 'html'"
        v-model="htmlMode"
        label="HTML view"
        :options="[{ value: 'preview', label: 'Preview' }, { value: 'source', label: 'Source' }]"
        class="self-start"
      />
      <p v-if="busy" role="status">Loading preview…</p>
      <p v-if="error" role="alert" class="text-sm text-destructive">
        {{ error }}
      </p>
      <ContentView
        v-if="previewText !== null"
        :text="previewText"
        :media-type="kind === 'markdown' ? 'text/markdown' : kind === 'html' && htmlMode === 'preview' ? 'text/html' : 'text/plain'"
        :maximum-characters="8388608"
      />
      <iframe
        v-if="previewUrl && kind === 'pdf'"
        :src="previewUrl"
        :title="attachment.filename"
        class="h-[70dvh] w-full rounded-md border"
      />
    </DialogContent>
  </Dialog>
</template>
