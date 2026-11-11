<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref, watch } from "vue";
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
  imageUrl: string | null;
  previewImage: (source: string) => void;
}>();
const kind = computed(() =>
  attachmentView(props.attachment.filename, props.attachment.mediaType),
);
const canPreview = computed(() => kind.value !== "image" && kind.value !== "download");
const busy = ref(false),
  error = ref<string | null>(null),
  previewUrl = ref<string | null>(null),
  previewText = ref<string | null>(null),
  dialogOpen = ref(false),
  htmlMode = ref<"preview" | "source">("preview");
let controller: AbortController | undefined;
let disposed = false;
const releasePreview = () => {
  controller?.abort(); controller = undefined;
  if (previewUrl.value) URL.revokeObjectURL(previewUrl.value);
  previewUrl.value = null;
  previewText.value = null;
  busy.value = false;
  error.value = null;
};
const loadPreview = async () => {
  const work = controller = new AbortController();
  busy.value = true;
  error.value = null;
  try {
    const value = await attachmentBytes(props.attachment, props.taskId, work.signal);
    if (work.signal.aborted || controller !== work || !dialogOpen.value) return;
    if (kind.value === "markdown" || kind.value === "text" || kind.value === "html")
      previewText.value = new TextDecoder().decode(value);
    else
      previewUrl.value = URL.createObjectURL(
        new Blob([value], { type: props.attachment.mediaType }),
      );
  } catch (cause) {
    if (!work.signal.aborted && controller === work) error.value =
      cause instanceof Error
        ? cause.message
        : "The saved attachment is unavailable.";
  } finally {
    if (controller === work) { controller = undefined; busy.value = false; }
  }
};
const openPreview = () => {
  if (busy.value) return;
  dialogOpen.value = true;
};
watch(dialogOpen, open => {
  if (open) void loadPreview();
  else releasePreview();
}, { flush: "sync" });
const download = async () => {
  if (busy.value) return;
  const work = controller = new AbortController();
  busy.value = true;
  error.value = null;
  try {
    const value = await attachmentBytes(props.attachment, props.taskId, work.signal);
    if (!work.signal.aborted && controller === work) saveBytes(value, props.attachment.filename);
  } catch (cause) {
    if (!work.signal.aborted && controller === work) error.value =
      cause instanceof Error
        ? cause.message
        : "The saved attachment is unavailable.";
  } finally {
    if (controller === work) { controller = undefined; busy.value = false; }
  }
};
onMounted(async () => {
  if (kind.value !== "image") return;
  busy.value = true;
  try {
    if (!await props.loadImage(attachmentSource(props.attachment.attachmentId))) throw new Error("The saved attachment is unavailable.");
  } catch (cause) {
    if (!disposed) error.value = cause instanceof Error ? cause.message : "The saved attachment is unavailable.";
  } finally {
    if (!disposed) busy.value = false;
  }
});
onBeforeUnmount(() => {
  disposed = true;
  releasePreview();
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
          v-if="canPreview"
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
        v-if="canPreview"
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
      v-if="busy && kind === 'image'"
      role="status"
      class="text-sm text-muted-foreground"
    >
      Loading preview…
    </p>
    <button
      v-if="imageUrl && kind === 'image'"
      type="button"
      class="block max-w-full rounded-md focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
      :aria-label="'Enlarge ' + attachment.filename"
      @click="previewImage(attachmentSource(attachment.attachmentId))"
    ><img :src="imageUrl" :alt="attachment.filename" class="max-h-80 max-w-full rounded-md object-contain" /></button>
    <Button v-if="kind === 'image' && !imageUrl && !busy" type="button" variant="outline" size="sm" :aria-label="'Enlarge ' + attachment.filename" @click="previewImage(attachmentSource(attachment.attachmentId))">Preview image</Button>
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
      <video
        v-if="previewUrl && kind === 'video'"
        :src="previewUrl"
        controls
        preload="metadata"
        class="max-h-[70dvh] max-w-full rounded-md"
        :aria-label="attachment.filename"
      />
    </DialogContent>
  </Dialog>
</template>
