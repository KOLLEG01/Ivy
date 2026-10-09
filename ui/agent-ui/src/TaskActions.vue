<script setup lang="ts">
import { computed, ref, useId, watch } from "vue";
import { Archive, FolderOpen, Trash2 } from "@lucide/vue";
import {
  Button,
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
  Input,
  Label,
} from "@ivy/ui";
import type { BoundTool } from "../../../packages/sdk/src/client.js";
import { route } from "../../../packages/ui-client/src/runtime";
import { optionalTool, supportsFields, text } from "../../../packages/ui-client/src/native";
import { useNativeAction } from "../../../packages/ui-client/src/native-action";
import { useObjectArchive } from "../../../packages/ui-client/src/object-archive";
import NativeActionState from "../../../packages/ui-client/src/NativeActionState.vue";
import TaskProjectDialog from "../../../packages/ui-client/src/TaskProjectDialog.vue";
import { base, client, tr } from "./runtime";

// The same task menu in the task toolbar and on every sidebar row. `scope` keeps the retained
// actions of both places apart; capabilities are read when the menu first opens.
const props = defineProps<{
  node: string;
  threadId: string;
  name: string;
  projectId: string;
  archived: boolean;
  working: boolean;
  scope: string;
  align?: "start" | "end";
}>();
const emit = defineEmits<{ changed: [] }>();
const open = ref(false), id = useId();
type Tool = BoundTool | undefined;
interface Capabilities { rename?: Tool; archive?: Tool; unarchive?: Tool; remove?: Tool; project?: Tool }
const capabilities = ref<Capabilities | null>(null);
let loading: Promise<void> | null = null;
const loadCapabilities = () => loading ??= Promise.all(
  ["codex.thread/name/set", "codex.thread/archive", "codex.thread/unarchive", "codex.thread/delete", "codex.thread/metadata/update"]
    .map(name => optionalTool(client, props.node, name)),
).then(([rename, archive, unarchive, remove, project]) => {
  capabilities.value = { rename, archive, unarchive, remove, project };
}).catch(() => { loading = null; capabilities.value = {}; });
watch(open, value => { if (value) { taskName.value = props.name; taskNameDirty.value = false; void loadCapabilities(); } });

const key = ":" + base.href + props.scope + ":" + props.node + ":" + props.threadId;
const action = useNativeAction(client, "ivy:agent-task-menu" + key);
// A Codex task that works on a TaskBoard ticket is archived through its ticket, which also archives
// every Codex task of that ticket; the board then never shows finished work that was archived here.
const ticketArchive = useObjectArchive(client, "ivy:agent-ticket-archive" + key);
const owningTicket = async () =>
  (await client.request("objects.query", {
    contractKey: "task-board/task",
    where: { op: "and", args: [
      { op: "eq", field: "data:/primaryResourceRef/serviceNodeId", value: props.node },
      { op: "eq", field: "data:/primaryResourceRef/nativeId", value: props.threadId },
    ] },
    includeArchived: true,
    limit: 1,
  })).items[0]?.objectId ?? null;

const archivedOverride = ref<boolean | null>(null);
watch(() => props.archived, () => { archivedOverride.value = null; });
const archived = computed(() => archivedOverride.value ?? props.archived);
const taskName = ref(props.name), taskNameDirty = ref(false);
watch(() => props.name, value => { if (!taskNameDirty.value) taskName.value = value; });

// Outcomes that need attention open a dialog, so a failure is visible after the menu has closed.
const problemOpen = ref(false);
const settle = () => {
  if (action.saved.value?.phase === "succeeded" && !action.error.value) emit("changed");
  else if (!deleteOpen.value) problemOpen.value = true;
};
const rename = async () => {
  const binding = capabilities.value?.rename, name = taskName.value.trim();
  if (!binding || !name || action.locked.value) return;
  await action.start("Rename task", binding, { threadId: props.threadId, name });
  if (action.saved.value?.phase === "succeeded") taskNameDirty.value = false;
  settle();
};
const setArchived = async () => {
  const wasArchived = archived.value,
    binding = wasArchived ? capabilities.value?.unarchive : capabilities.value?.archive;
  if (action.locked.value || ticketArchive.busy.value) return;
  ticketArchive.clearError();
  const ticket = await owningTicket().catch(() => null);
  if (ticket) {
    if (await ticketArchive.run(ticket, !wasArchived)) { archivedOverride.value = !wasArchived; emit("changed"); }
    else problemOpen.value = true;
    return;
  }
  if (!binding) return;
  await action.start(wasArchived ? "Restore task" : "Archive task", binding, { threadId: props.threadId });
  if (action.saved.value?.phase === "succeeded") archivedOverride.value = !wasArchived;
  settle();
};
// Deleting removes the native conversation; a TaskBoard ticket keeps its task and is archived instead.
const deleteOpen = ref(false), deleteError = ref("");
const deleteTask = async () => {
  const binding = capabilities.value?.remove;
  if (!binding || action.locked.value) return;
  deleteError.value = "";
  if (await owningTicket().catch(() => null)) {
    deleteError.value = tr("Dieser Task gehört zu einem TaskBoard-Ticket. Archiviere ihn stattdessen.",
      "This task belongs to a TaskBoard ticket. Archive it instead.");
    return;
  }
  await action.start("Delete task", binding, { threadId: props.threadId });
  if (action.saved.value?.phase === "succeeded") {
    deleteOpen.value = false;
    const current = new URLSearchParams(location.hash.split("?")[1] ?? "");
    if (location.hash.startsWith("#/task") && current.get("node") === props.node && current.get("id") === props.threadId)
      location.hash = route("host", { node: props.node });
    emit("changed");
  } else deleteError.value = action.error.value || text(action.saved.value?.detail) ||
    tr("Der Task konnte nicht gelöscht werden.", "The task could not be deleted.");
};
const projectOpen = ref(false);
</script>

<template>
  <DropdownMenu v-model:open="open">
    <DropdownMenuTrigger as-child><slot /></DropdownMenuTrigger>
    <DropdownMenuContent :align="align ?? 'end'" class="w-72">
      <div class="space-y-2 p-2">
        <Label :for="id">Task name</Label>
        <div class="flex gap-2">
          <Input :id="id" v-model="taskName" :disabled="action.locked.value || !capabilities?.rename" maxlength="512"
            @input="taskNameDirty = true" @keydown.stop="$event.key === 'Enter' && rename()" />
          <Button variant="outline" :disabled="action.locked.value || !taskName.trim() || !taskNameDirty || !capabilities?.rename"
            @click="rename">Rename</Button>
        </div>
      </div>
      <DropdownMenuSeparator />
      <DropdownMenuItem v-if="supportsFields(capabilities?.project, ['threadId', 'projectId'])"
        :disabled="action.locked.value || working" @select="projectOpen = true">
        <FolderOpen aria-hidden="true" />{{ tr("Projekt ändern…", "Change project…") }}
      </DropdownMenuItem>
      <DropdownMenuItem :disabled="action.locked.value || ticketArchive.busy.value || !(archived ? capabilities?.unarchive : capabilities?.archive)"
        @select="setArchived">
        <Archive aria-hidden="true" />{{ archived ? "Restore" : "Archive" }}
      </DropdownMenuItem>
      <slot name="items" />
      <template v-if="capabilities?.remove">
        <DropdownMenuSeparator />
        <DropdownMenuItem variant="destructive" :disabled="action.locked.value || working"
          @select="deleteError = ''; deleteOpen = true">
          <Trash2 aria-hidden="true" />{{ tr("Löschen…", "Delete…") }}
        </DropdownMenuItem>
      </template>
    </DropdownMenuContent>
  </DropdownMenu>
  <Dialog v-model:open="deleteOpen">
    <DialogContent>
      <DialogHeader>
        <DialogTitle>{{ tr("Task löschen?", "Delete task?") }}</DialogTitle>
        <DialogDescription>{{ tr(
          "Die Unterhaltung wird dauerhaft aus Codex entfernt. Dateien in ihrem Arbeitsverzeichnis bleiben erhalten.",
          "The conversation is permanently removed from Codex. Files in its working directory are kept.",
        ) }}</DialogDescription>
      </DialogHeader>
      <p v-if="deleteError" role="alert" class="text-sm text-destructive">{{ deleteError }}</p>
      <DialogFooter>
        <Button variant="outline" :disabled="action.locked.value" @click="deleteOpen = false">{{ tr("Abbrechen", "Cancel") }}</Button>
        <Button variant="destructive" :disabled="action.locked.value" :loading="action.busy.value" @click="deleteTask">{{ tr("Löschen", "Delete") }}</Button>
      </DialogFooter>
    </DialogContent>
  </Dialog>
  <Dialog v-model:open="problemOpen">
    <DialogContent>
      <DialogHeader>
        <DialogTitle>{{ name }}</DialogTitle>
        <DialogDescription>{{ action.saved.value?.label ?? tr("Task-Aktion", "Task action") }}</DialogDescription>
      </DialogHeader>
      <p v-if="ticketArchive.error.value" role="alert" class="text-sm text-destructive">{{ ticketArchive.error.value }}</p>
      <NativeActionState :action="action.saved.value" :busy="action.busy.value" :error="action.error.value"
        @reconcile="async () => { await action.reconcile(); if (action.saved.value?.phase === 'succeeded') { problemOpen = false; emit('changed'); } }"
        @retry="async () => { await action.retry(); settle(); }" />
      <DialogFooter>
        <Button variant="outline" @click="problemOpen = false">{{ tr("Schließen", "Close") }}</Button>
      </DialogFooter>
    </DialogContent>
  </Dialog>
  <TaskProjectDialog
    v-if="projectOpen && capabilities?.project"
    v-model:open="projectOpen"
    :client="client"
    :node="node"
    :thread-id="threadId"
    :project-id="projectId"
    :binding="capabilities.project"
    :scope="base.href"
    @changed="emit('changed')"
  />
</template>
