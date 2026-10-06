<script setup lang="ts">
import { ChangeHistory, PageControls, RemoteState } from "@ivy/ui";
import type { ChangeEntry, FieldChange } from "@ivy/ui";
import { usePage } from "../../../packages/ui-client/src/runtime";
import { client, readDocument } from "./runtime";
import type { Document, TaskBoard } from "./runtime";
import { priorityLabel, stateLabel } from "./board";

/** Field-level history of one Task, derived by comparing its consecutive object revisions. */
const props = defineProps<{ taskId: string; workspace: TaskBoard.WorkspaceInfo }>();
const cache = new Map<string, Promise<unknown>>();
const cached = <K extends "task" | "history">(key: K, pin: TaskBoard.ObjectPin | string) => {
  const id = key + JSON.stringify(pin);
  if (!cache.has(id))
    // History records may belong to their operation rather than the workspace root.
    cache.set(id, readDocument(key, pin, key === "task" ? props.workspace.rootObjectId : undefined).catch(() => null));
  return cache.get(id) as Promise<Document<K> | null>;
};
const actors: Record<string, string> = { user: "You", worker: "Agent", native: "Agent", scheduler: "TaskBoard", recovery: "TaskBoard" };
const contacts: Record<string, string> = { ticket: "Ticket only", chat: "Ticket and chat", phone: "Ticket and call" };
const date = (value: string | null) => (value ? new Date(value).toLocaleString() : "");
const host = (value: TaskBoard.ExecutionRequirement | null) =>
  value?.kind === "host" ? value.hostId : value?.kind === "automatic" ? "Any host" : "Default";
const location = (value: TaskBoard.WorkspaceRequirement) =>
  value.kind === "task_workspace" ? "No project"
    : value.kind === "existing_project" ? value.projectId + (value.path ? " · " + value.path : "") + (value.useWorktree ? " · worktree" : "")
      : value.kind === "directory_path" ? value.path
        : value.kind === "repository_path" ? "New project " + value.folderName + " from " + value.repositoryUrl
          : "New project " + value.folderName;
const taskKeys = async (ids: string[]) =>
  (await Promise.all(ids.map(async (id) => (await cached("task", id))?.value.taskKey ?? id))).join(", ");
async function changes(before: TaskBoard.Task, after: TaskBoard.Task): Promise<FieldChange[]> {
  const a = before.fields, b = after.fields, result: FieldChange[] = [];
  const add = (field: string, from: string, to: string, long = false) => {
    if (from !== to) result.push({ field, before: from, after: to, ...(long ? { long } : {}) });
  };
  add("Status", stateLabel(before.workflowState), stateLabel(after.workflowState));
  add("Title", a.title, b.title);
  add("Description", a.description, b.description, true);
  add("Acceptance criteria", a.acceptanceCriteria.join("\n"), b.acceptanceCriteria.join("\n"), true);
  add("Worker", a.control === "agent" ? "Agent" : "Me", b.control === "agent" ? "Agent" : "Me");
  add("Priority", priorityLabel(a.priority), priorityLabel(b.priority));
  add("Category", a.category ?? "", b.category ?? "");
  add("Dependencies", (a.requiredCapabilities ?? []).join(", "), (b.requiredCapabilities ?? []).join(", "));
  add("Host", host(a.executionRequirement), host(b.executionRequirement));
  add("Project", location(a.workspaceRequirement), location(b.workspaceRequirement));
  add("Model", a.nativeOptions?.model ?? "Default", b.nativeOptions?.model ?? "Default");
  add("Reasoning", a.nativeOptions?.reasoningEffort ?? "Default", b.nativeOptions?.reasoningEffort ?? "Default");
  add("Speed", a.nativeOptions?.serviceTier ?? "Default", b.nativeOptions?.serviceTier ?? "Default");
  if (JSON.stringify(a.dependencies) !== JSON.stringify(b.dependencies))
    add("Blockers", await taskKeys(a.dependencies), await taskKeys(b.dependencies));
  add("Updates", contacts[a.userContact] ?? a.userContact, contacts[b.userContact] ?? b.userContact);
  add("Project scheduling", a.allowParallel ? "Parallel" : "Sequential", b.allowParallel ? "Parallel" : "Sequential");
  add("Due", date(a.dueAt), date(b.dueAt));
  add("Next review", date(a.nextReviewAt), date(b.nextReviewAt));
  return result;
}
// Each action first reserves the Task (publication set) and then publishes it with its history link,
// so changes are compared between consecutive published revisions.
const history = usePage(async (signal, cursor) => {
  const page = await client.request("objects.history", { objectId: props.taskId, limit: 20, ...(cursor ? { cursor } : {}) }, { signal });
  const read = (revision: number) => cached("task", { objectId: props.taskId, revision });
  const previousPublished = async (revision: number) => {
    for (let index = revision - 1; index >= 1; index--) {
      const document = await read(index);
      if (document && !document.value.publication) return document;
    }
    return null;
  };
  const revisions = page.items.map((item) => item.revision);
  for (let index = 0; index < revisions.length; index += 8)
    await Promise.all(revisions.slice(index, index + 8).map(read));
  signal.throwIfAborted();
  const entries: ChangeEntry[] = [];
  for (const item of page.items) {
    const after = await read(item.revision);
    if (!after || after.value.publication) continue;
    const before = await previousPublished(item.revision),
      pin = after.value.lastHistory,
      record = pin && pin.objectId !== before?.value.lastHistory?.objectId ? await cached("history", pin) : null,
      actor = record ? actors[record.value.actor.source] ?? "TaskBoard" : "TaskBoard";
    if (!before) {
      entries.push({ id: String(item.revision), actor, at: item.createdAt, summary: "created the task", changes: [] });
      continue;
    }
    const fields = await changes(before.value, after.value);
    if (fields.length) entries.push({ id: String(item.revision), actor, at: item.createdAt, changes: fields });
  }
  signal.throwIfAborted();
  return { items: entries, nextCursor: page.nextCursor };
}, 0, undefined, ["objects/task-board/task"]);
</script>
<template>
  <div class="space-y-4">
    <RemoteState
      :loading="history.loading.value"
      :error="history.error.value"
      :has-data="!!history.value.value"
      @retry="history.refresh"
    />
    <p
      v-if="history.value.value && !history.value.value.items.length"
      class="text-sm text-muted-foreground"
    >
      No field changes {{ history.page.value > 1 ? "on this page" : "yet" }}.
    </p>
    <ChangeHistory v-else-if="history.value.value" :entries="history.value.value.items" />
    <PageControls
      v-if="history.page.value > 1 || history.value.value?.nextCursor"
      :page="history.page.value"
      :count="history.value.value?.items.length ?? 0"
      :has-next="!!history.value.value?.nextCursor"
      :loading="history.loading.value"
      @previous="history.previous"
      @next="history.next"
    />
  </div>
</template>
