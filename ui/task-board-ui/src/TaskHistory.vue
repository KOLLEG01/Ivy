<script setup lang="ts">
import { ChangeHistory, PageControls, RemoteState } from "@ivy/ui";
import type { FieldChange } from "@ivy/ui";
import { IvyError, usePage } from "../../../packages/ui-client/src/runtime";
import { client, readDocument, readTaskFields, tr } from "./runtime";
import type { TaskBoard } from "./runtime";
import { readFieldHistory, type TaskHistoryFields } from './field-history';
import { priorityLabel, stateLabel } from "./board";

/** Field-level history of one Task, derived by comparing its consecutive object revisions. */
const props = defineProps<{ taskId: string; workspace: TaskBoard.WorkspaceInfo }>();
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
async function changes(before: TaskHistoryFields, after: TaskHistoryFields): Promise<FieldChange[]> {
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
  const account = (value: TaskBoard.NativeOptions | undefined) => value?.backend
    ? [value.backend.provider, value.backend.backendId, value.backend.accountId].join(' / ') : 'Default';
  add("Account", account(a.nativeOptions), account(b.nativeOptions));
  add("Reasoning", a.nativeOptions?.reasoningEffort ?? "Default", b.nativeOptions?.reasoningEffort ?? "Default");
  add("Speed", a.nativeOptions?.serviceTier ?? "Default", b.nativeOptions?.serviceTier ?? "Default");
  add("Next appointment", date(before.wakeAt), date(after.wakeAt));
  add("Watched tickets", before.dependencies.join(", "), after.dependencies.join(", "));
  add("Updates", contacts[a.userContact] ?? a.userContact, contacts[b.userContact] ?? b.userContact);
  add("Project scheduling", a.allowParallel ? "Parallel" : "Sequential", b.allowParallel ? "Parallel" : "Sequential");
  add("Due", date(a.dueAt), date(b.dueAt));
  return result;
}
// Each action first reserves the Task (publication set) and then publishes it with its history link,
// so changes are compared between consecutive published revisions.
const history = usePage(async (signal, cursor) => {
  const retained = async <T,>(read: () => Promise<T>): Promise<T | null> => {
    try { return await read(); }
    catch (error) {
      signal.throwIfAborted();
      if (error instanceof IvyError && ['not_found', 'revision_pruned'].includes(error.code)) return null;
      throw error;
    }
  };
  return readFieldHistory({
    page: cursor => client.request('objects.history', { objectId: props.taskId, limit: 20, ...(cursor ? { cursor } : {}) }, { signal }),
    fields: revision => retained(() => readTaskFields({ objectId: props.taskId, revision }, props.workspace.rootObjectId, signal)),
    actor: async pin => {
      const record = await retained(() => readDocument('history', pin, undefined, signal));
      return record ? actors[record.value.actor.source] ?? 'TaskBoard' : null;
    },
    changes,
  }, signal, cursor);
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
    <p v-if="history.value.value?.partial" class="text-sm text-muted-foreground" role="note">
      {{ tr('Ein Teil des Verlaufs ist nicht mehr verfügbar. Vergleiche verwenden die erhaltenen Versionen.', 'Some history is no longer available. Comparisons use retained versions.') }}
    </p>
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
