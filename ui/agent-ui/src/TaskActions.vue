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
import type { Agent, BoundTool } from "../../../packages/sdk/src/client.js";
import { nativeMethod, sameBackend } from '../../../packages/ui-client/src/native-backends';
import { nativeRouteQuery, nativeRouteOwner, nativeOwnerKey, nativeStorageOwner } from '../../../packages/ui-client/src/native-route';
import { canonical } from '../../../packages/sdk/src/client.js';
import { record } from '../../../packages/ui-client/src/native';
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
  available: boolean;
  backend?: Agent.BackendIdentity | undefined;
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
const loadCapabilities = () => { if (!props.available) return; return loading ??= Promise.all(
  ["thread/name/set", "thread/archive", "thread/unarchive", "thread/delete", "thread/metadata/update"]
    .map(name => optionalTool(client, props.node, nativeMethod(props.backend, name))),
).then(([rename, archive, unarchive, remove, project]) => {
  capabilities.value = { rename, archive, unarchive, remove, project };
}).catch(() => { loading = null; capabilities.value = {}; }); };
watch(open, value => { if (value) { taskName.value = props.name; taskNameDirty.value = false; void loadCapabilities(); } });

const owner = { serviceNodeId: props.node, namespace: props.backend?.backendId ?? 'codex', backend: props.backend };
const key = ":" + base.href + props.scope + ":" + nativeStorageOwner(props.node, props.backend) + ":" + props.threadId;
const action = useNativeAction(client, "ivy:agent-task-menu" + key, () => props.backend);
// A Codex task that works on a TaskBoard ticket is archived through its ticket, which also archives
// every Codex task of that ticket; the board then never shows finished work that was archived here.
const ticketArchive = useObjectArchive(client, "ivy:agent-ticket-archive" + key);
const owningTicket = async () => {
  const id = (await client.request("objects.query", {
    contractKey: "task-board/task",
    where: { op: "and", args: [
      { op: "eq", field: "data:/primaryResourceRef/serviceNodeId", value: props.node },
      { op: "eq", field: "data:/primaryResourceRef/namespace", value: owner.namespace },
      { op: "eq", field: "data:/primaryResourceRef/nativeId", value: props.threadId },
    ] },
    includeArchived: true,
    limit: 1,
  })).items[0]?.objectId;
  if (!id) return null;
  const saved = await client.request('objects.read', { objectId: id });
  if (saved.object.contractKey !== 'task-board/task' || saved.content.encoding !== 'json') throw new Error('The owning ticket could not be verified.');
  const task = record(saved.content.value), pin = record(task.lastRun);
  if (canonical(task.primaryResourceRef) !== canonical({ serviceNodeId: props.node, namespace: owner.namespace, kind: 'thread', nativeId: props.threadId }))
    throw new Error('The ticket conversation changed. Refresh it before archiving.');
  if (props.backend) {
    if (!pin.objectId || typeof pin.revision !== 'number') throw new Error('The ticket account has no retained execution proof.');
    const run = await client.request('objects.read', { objectId: String(pin.objectId), revision: pin.revision });
    if (run.object.contractKey !== 'task-board/run' || run.content.encoding !== 'json' ||
        !sameBackend(record(record(run.content.value).target).backend as Agent.BackendIdentity | undefined, props.backend) ||
        canonical(record(run.content.value).primaryResourceRef) !== canonical(task.primaryResourceRef))
      throw new Error('The ticket belongs to another account. Open the ticket to manage it.');
  }
  return id;
};

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
  if (!props.available || !binding || !name || action.locked.value) return;
  await action.start("Rename task", binding, { threadId: props.threadId, name });
  if (action.saved.value?.phase === "succeeded") taskNameDirty.value = false;
  settle();
};
const setArchived = async () => {
  const wasArchived = archived.value,
    binding = wasArchived ? capabilities.value?.unarchive : capabilities.value?.archive;
  if (!props.available || action.locked.value || ticketArchive.busy.value) return;
  ticketArchive.clearError();
  let ticket;
  try { ticket = await owningTicket(); }
  catch (cause) { action.error.value = cause instanceof Error ? cause.message : String(cause); problemOpen.value = true; return; }
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
  if (!props.available || !binding || action.locked.value) return;
  deleteError.value = "";
  let ticket;
  try { ticket = await owningTicket(); }
  catch (cause) { deleteError.value = cause instanceof Error ? cause.message : String(cause); return; }
  if (ticket) {
    deleteError.value = tr("Dieser Task gehört zu einem TaskBoard-Ticket. Archiviere ihn stattdessen.",
      "This task belongs to a TaskBoard ticket. Archive it instead.");
    return;
  }
  await action.start("Delete task", binding, { threadId: props.threadId });
  if (action.saved.value?.phase === "succeeded") {
    deleteOpen.value = false;
    const current = new URLSearchParams(location.hash.split("?")[1] ?? "");
    if (location.hash.startsWith("#/task") && nativeOwnerKey(nativeRouteOwner(current)) === nativeOwnerKey(owner) && current.get("id") === props.threadId)
      location.hash = route("host", nativeRouteQuery(owner));
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
          "Die Unterhaltung wird dauerhaft aus diesem Account entfernt. Dateien in ihrem Arbeitsverzeichnis bleiben erhalten.",
          "The conversation is permanently removed from this account. Files in its working directory are kept.",
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
    :backend="backend"
    :thread-id="threadId"
    :project-id="projectId"
    :binding="capabilities.project"
    :scope="base.href"
    @changed="emit('changed')"
  />
</template>
