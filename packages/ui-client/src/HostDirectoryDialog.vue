<script setup lang="ts">
import { computed, ref, watch } from "vue";
import {
  Alert,
  AlertDescription,
  Button,
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
  Input,
  Skeleton,
} from "@ivy/ui";
import { ArrowUp, Folder } from "@lucide/vue";
import type { RpcClient } from "../../sdk/src/client.js";
import { list, nativeRead, record, text } from "./native";

/** Browse directories on one AgentManager host and choose an absolute folder. */
const open = defineModel<boolean>("open", { default: false });
const props = defineProps<{
  client: RpcClient;
  node: string;
  host: string;
  start: string;
  title?: string;
}>();
const emit = defineEmits<{ select: [path: string] }>();
const path = ref(""),
  typed = ref(""),
  folders = ref<string[]>([]),
  loading = ref(false),
  error = ref("");
const separator = computed(() =>
  /^[A-Za-z]:\\/.test(path.value) ? "\\" : "/",
);
const parent = computed(() => {
  const trimmed = path.value.replace(/[\\/]+$/, "");
  const index = Math.max(trimmed.lastIndexOf("/"), trimmed.lastIndexOf("\\"));
  if (index < 0) return "";
  const value = trimmed.slice(0, index);
  return /^[A-Za-z]:$/.test(value) ? value + "\\" : value || "/";
});
const child = (name: string) =>
  path.value.replace(/[\\/]+$/, "") + separator.value + name;
const parentOf = (value: string) => {
  const trimmed = value.replace(/[\\/]+$/, "");
  const index = Math.max(trimmed.lastIndexOf("/"), trimmed.lastIndexOf("\\"));
  return index <= 0 ? "" : trimmed.slice(0, index);
};
let current: AbortController | null = null;
// The first folder may not exist yet (a project root is created on first use); show its nearest parent.
async function load(target: string, nearest = false) {
  current?.abort();
  const controller = (current = new AbortController());
  loading.value = true;
  error.value = "";
  try {
    const value = record(
      await nativeRead(
        props.client,
        props.node,
        "codex.fs/readDirectory",
        { path: target },
        controller.signal,
      ),
    );
    folders.value = list(value.entries)
      .map(record)
      .filter((entry) => entry.isDirectory === true)
      .map((entry) => text(entry.fileName))
      .filter((name) => name && !name.startsWith("."))
      .sort((a, b) => a.localeCompare(b));
    path.value = typed.value = target;
  } catch (cause) {
    if (controller.signal.aborted) return;
    const parent = nearest ? parentOf(target) : "";
    if (parent) return void load(parent, true);
    error.value = cause instanceof Error ? cause.message : String(cause);
  } finally {
    if (current === controller) loading.value = false;
  }
}
watch(
  open,
  (value) => {
    if (value) void load(props.start || "/", true);
    else current?.abort();
  },
  { immediate: true },
);
const choose = () => {
  emit("select", path.value);
  open.value = false;
};
</script>
<template>
  <Dialog v-model:open="open">
    <DialogContent class="flex max-h-[min(40rem,90dvh)] flex-col sm:max-w-lg">
      <DialogHeader>
        <DialogTitle>{{ title ?? "Add project" }}</DialogTitle>
        <DialogDescription>Choose a folder on {{ host }}.</DialogDescription>
      </DialogHeader>
      <form class="flex gap-2" @submit.prevent="load(typed.trim())">
        <Button
          type="button"
          variant="outline"
          size="icon"
          :disabled="!parent || loading"
          aria-label="Parent folder"
          @click="load(parent)"
          ><ArrowUp aria-hidden="true"
        /></Button>
        <Input
          v-model="typed"
          aria-label="Folder path"
          class="min-w-0 flex-1 font-mono text-xs"
        />
        <Button type="submit" variant="outline" :disabled="!typed.trim()"
          >Open</Button
        >
      </form>
      <Alert v-if="error" variant="destructive"
        ><AlertDescription>{{ error }}</AlertDescription></Alert
      >
      <div
        class="min-h-0 flex-1 overflow-y-auto rounded-md border p-1"
        aria-label="Folders"
        role="list"
      >
        <template v-if="loading && !folders.length"
          ><Skeleton v-for="index in 4" :key="index" class="m-1 h-7"
        /></template>
        <p
          v-else-if="!folders.length"
          class="px-2 py-6 text-center text-sm text-muted-foreground"
        >
          No subfolders.
        </p>
        <button
          v-for="name in folders"
          :key="name"
          type="button"
          role="listitem"
          class="flex w-full items-center gap-2 rounded-sm px-2 py-1.5 text-left text-sm hover:bg-accent hover:text-accent-foreground focus-visible:bg-accent focus-visible:outline-none"
          @click="load(child(name))"
        >
          <Folder class="size-4 shrink-0 text-muted-foreground" aria-hidden="true" />
          <span class="truncate">{{ name }}</span>
        </button>
      </div>
      <DialogFooter>
        <Button type="button" variant="ghost" @click="open = false"
          >Cancel</Button
        >
        <Button type="button" :disabled="!path || loading" @click="choose"
          >Use this folder</Button
        >
      </DialogFooter>
    </DialogContent>
  </Dialog>
</template>
