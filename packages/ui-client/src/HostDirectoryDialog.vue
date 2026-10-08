<script setup lang="ts">
import { onBeforeUnmount, ref, watch } from "vue";
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
import { IvyError, serviceTools } from "../../sdk/src/client.js";
import type { Agent, RpcClient } from "../../sdk/src/client.js";

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
  folders = ref<Agent.DirectoryListResult["directories"]>([]),
  parent = ref(""),
  loading = ref(false),
  error = ref("");
const parentOf = (value: string) => {
  const trimmed = value.replace(/[\\/]+$/, "");
  const index = Math.max(trimmed.lastIndexOf("/"), trimmed.lastIndexOf("\\"));
  const parent = index === 0 && trimmed.startsWith('/') ? '/' : index < 0 ? "" : trimmed.slice(0, index);
  return /^[A-Za-z]:$/.test(parent) ? parent + "\\" : parent;
};
let current: AbortController | null = null;
// The first folder may not exist yet (a project root is created on first use); show its nearest parent.
async function load(target: string, nearest = false) {
  current?.abort();
  const controller = (current = new AbortController());
  loading.value = true;
  error.value = "";
  try {
    const value = (await serviceTools(props.client, props.node, [
      { namespace: "agent", interfaceVersion: "1.0.0" },
    ]).read(
      "agent.listDirectories",
      { path: target },
      {
        signal: controller.signal,
      },
    )) as Agent.DirectoryListResult;
    if (controller.signal.aborted) return;
    folders.value = value.directories;
    parent.value = value.parent ?? "";
    path.value = typed.value = value.path;
  } catch (cause) {
    if (controller.signal.aborted) return;
    const parent = nearest && cause instanceof IvyError && cause.code === 'ENOENT' ? parentOf(target) : "";
    if (parent) return void load(parent, true);
    typed.value = path.value || target;
    const message = cause instanceof Error ? cause.message : String(cause);
    error.value = `Could not open ${target}. ${message}`;
  } finally {
    if (current === controller) loading.value = false;
  }
}
watch(
  open,
  (value) => {
    if (value) {
      path.value = typed.value = parent.value = "";
      folders.value = [];
      void load(props.start || "/", true);
    }
    else current?.abort();
  },
  { immediate: true },
);
onBeforeUnmount(() => current?.abort());
const choose = () => {
  if (!path.value || loading.value) return;
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
        <Button
          v-for="folder in folders"
          :key="folder.path"
          type="button"
          variant="ghost"
          role="listitem"
          class="w-full justify-start font-normal"
          :disabled="loading"
          @click="load(folder.path)"
        >
          <Folder
            class="size-4 shrink-0 text-muted-foreground"
            aria-hidden="true"
          />
          <span class="truncate">{{ folder.name }}</span>
        </Button>
      </div>
      <DialogFooter>
        <Button type="button" variant="ghost" @click="open = false"
          >Cancel</Button
        >
        <Button
          type="button"
          :disabled="!path || loading"
          @click="choose"
          >Use this folder</Button
        >
      </DialogFooter>
    </DialogContent>
  </Dialog>
</template>
