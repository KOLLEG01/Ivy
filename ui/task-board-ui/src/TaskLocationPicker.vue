<script setup lang="ts">
import { computed, ref } from "vue";
import {
  Button,
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
  Field,
  FieldDescription,
  FieldError,
  Input,
  Label,
  ProjectSelect,
} from "@ivy/ui";
import HostDirectoryDialog from "../../../packages/ui-client/src/HostDirectoryDialog.vue";
import { client, tr } from "./runtime";
import type { TaskBoard } from "./runtime";
import type { ExecutionAgent } from "./execution-options";

/** Where agent work runs on the chosen host: no project, a known project, a folder or a new project. */
const model = defineModel<TaskBoard.WorkspaceRequirement>({ required: true });
const props = defineProps<{
  id?: string;
  host: string;
  agent: ExecutionAgent | undefined;
  disabled?: boolean;
  defaultWorktree?: boolean;
}>();
const projectKey = (id: string, path: string) =>
  "project:" + JSON.stringify([id, path]);
const projects = computed(() =>
  (props.agent?.projects.projects ?? [])
    .filter((project) => project.kind !== "task")
    .flatMap((project) =>
      project.paths.map((path) => ({
        key: projectKey(project.nativeId, path),
        id: project.nativeId,
        name: project.name,
        path,
      })),
    ),
);
const projectRoot = computed(
  () => props.agent?.projects.defaults?.projectRoot ?? "",
);
const projectOptions = computed(() =>
  projects.value.map((project) => ({
    value: project.key,
    name: project.name,
    path: project.path,
  })),
);
const selected = computed(() => {
  const value = model.value;
  if (value.kind === "existing_project")
    return (
      projects.value.find(
        (project) =>
          project.id === value.projectId &&
          (!value.path || project.path === value.path),
      )?.key ?? projectKey(value.projectId, value.path ?? "")
    );
  if (value.kind === "directory_path") return "directory";
  if (value.kind === "new_project_path" || value.kind === "repository_path")
    return "new";
  return "none";
});
const selectedLabel = computed(() => {
  const value = model.value;
  if (value.kind === "existing_project")
    return (
      projects.value.find((project) => project.key === selected.value)?.name ??
      tr("Projekt nicht verfügbar", "Project unavailable")
    );
  if (value.kind === "directory_path") return value.path;
  if (value.kind === "new_project_path")
    return "New project · " + value.folderName;
  if (value.kind === "repository_path")
    return "New project · " + value.folderName + " from Git";
  return "No project";
});
const browsing = ref(false),
  creating = ref(false),
  folder = ref(""),
  repository = ref("");
const choose = (value: unknown) => {
  const key = String(value);
  if (key === "none") model.value = { kind: "task_workspace" };
  else if (key.startsWith("project:")) {
    const [projectId, path] = JSON.parse(key.slice(8)) as [string, string];
    model.value = {
      kind: "existing_project",
      projectId,
      path: path || null,
      useWorktree:
        model.value.kind === "existing_project"
          ? model.value.useWorktree
          : (props.defaultWorktree ?? false),
    };
  }
};
const newProject = () => {
  folder.value = "folderName" in model.value ? model.value.folderName : "";
  repository.value =
    model.value.kind === "repository_path" ? model.value.repositoryUrl : "";
  creating.value = true;
};
const folderInvalid = computed(
  () =>
    !!folder.value.trim() &&
    (/[\\/]/.test(folder.value) || [".", ".."].includes(folder.value.trim())),
);
// TaskBoard accepts HTTP(S) repositories without embedded credentials.
const repositoryInvalid = computed(
  () =>
    !!repository.value.trim() &&
    !/^https?:\/\/[^@/]+(?:\/|$)/.test(repository.value.trim()),
);
const createProject = () => {
  const folderName = folder.value.trim();
  if (!folderName || folderInvalid.value || repositoryInvalid.value) return;
  model.value = repository.value.trim()
    ? {
        kind: "repository_path",
        repositoryUrl: repository.value.trim(),
        folderName,
      }
    : { kind: "new_project_path", folderName };
  creating.value = false;
};
const nameFromRepository = () => {
  if (folder.value.trim()) return;
  const match = /([^/:]+?)(?:\.git)?\/?$/.exec(repository.value.trim());
  if (match) folder.value = match[1]!;
};
</script>
<template>
  <ProjectSelect
    :id="id"
    :model-value="selected"
    :selected-label="selectedLabel"
    :projects="projectOptions"
    :host="host"
    empty-value="none"
    empty-label="No project"
    :add-label="
      tr('Bestehenden Ordner auswählen…', 'Choose existing directory…')
    "
    :create-label="tr('Neues Projekt…', 'New project…')"
    :disabled="disabled"
    :actions-disabled="!agent"
    class="w-full"
    @update:model-value="choose"
    @add="browsing = true"
    @create="newProject"
  />
  <HostDirectoryDialog
    v-if="agent"
    v-model:open="browsing"
    :client="client"
    :node="agent.node.serviceNodeId"
    :host="host"
    :start="model.kind === 'directory_path' ? model.path : projectRoot"
    @select="model = { kind: 'directory_path', path: $event }"
  />
  <Dialog v-model:open="creating">
    <DialogContent class="sm:max-w-lg">
      <form class="grid gap-4" @submit.prevent="createProject">
        <DialogHeader>
          <DialogTitle>New project</DialogTitle>
          <DialogDescription
            >Created on {{ host }} when the agent starts.</DialogDescription
          >
        </DialogHeader>
        <Field
          ><Label for="new-project-repository">Git URL (optional)</Label
          ><Input
            id="new-project-repository"
            v-model="repository"
            placeholder="https://example.com/team/project.git"
            :aria-invalid="repositoryInvalid"
            @blur="nameFromRepository"
          /><FieldError v-if="repositoryInvalid"
            >Enter an HTTP(S) Git URL without credentials.</FieldError
          ></Field
        >
        <Field :data-invalid="folderInvalid"
          ><Label for="new-project-folder">Folder</Label
          ><Input
            id="new-project-folder"
            v-model="folder"
            required
            :aria-invalid="folderInvalid"
          /><FieldError v-if="folderInvalid"
            >Use one folder name without slashes.</FieldError
          ><FieldDescription v-else class="break-all"
            >Target path: {{ projectRoot || "project folder" }}/{{
              folder.trim() || "…"
            }}</FieldDescription
          ></Field
        >
        <DialogFooter>
          <Button type="button" variant="ghost" @click="creating = false"
            >Cancel</Button
          >
          <Button
            type="submit"
            :disabled="!folder.trim() || folderInvalid || repositoryInvalid"
            >Use new project</Button
          >
        </DialogFooter>
      </form>
    </DialogContent>
  </Dialog>
</template>
