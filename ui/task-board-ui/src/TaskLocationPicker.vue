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
  Select,
  SelectContent,
  SelectGroup,
  SelectItem,
  SelectLabel,
  SelectSeparator,
  SelectTrigger,
  SelectValue,
} from "@ivy/ui";
import { FolderGit2, FolderOpen, FolderPlus, House } from "@lucide/vue";
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
  if (value.kind === "new_project_path") return "New project · " + value.folderName;
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
  if (key === "@add") browsing.value = true;
  else if (key === "@new") {
    folder.value =
      "folderName" in model.value ? model.value.folderName : "";
    repository.value =
      model.value.kind === "repository_path" ? model.value.repositoryUrl : "";
    creating.value = true;
  } else if (key === "none") model.value = { kind: "task_workspace" };
  else if (key.startsWith("project:")) {
    const [projectId, path] = JSON.parse(key.slice(8)) as [string, string];
    model.value = {
      kind: "existing_project",
      projectId,
      path: path || null,
      useWorktree:
        model.value.kind === "existing_project" ? model.value.useWorktree : (props.defaultWorktree ?? false),
    };
  }
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
    ? { kind: "repository_path", repositoryUrl: repository.value.trim(), folderName }
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
  <Select
    :model-value="selected"
    :disabled="disabled"
    @update:model-value="choose"
  >
    <SelectTrigger :id="id" class="w-full" :data-value="selected"
      ><SelectValue
        ><span class="truncate">{{ selectedLabel }}</span></SelectValue
      ></SelectTrigger
    >
    <SelectContent>
      <SelectItem value="none"
        ><House aria-hidden="true" /><span>No project</span></SelectItem
      >
      <SelectItem v-if="selected === 'directory'" value="directory"
        ><FolderOpen aria-hidden="true" /><span class="truncate">{{
          selectedLabel
        }}</span></SelectItem
      >
      <SelectItem v-if="selected === 'new'" value="new"
        ><FolderPlus aria-hidden="true" /><span class="truncate">{{
          selectedLabel
        }}</span></SelectItem
      >
      <SelectItem
        v-if="
          model.kind === 'existing_project' &&
          !projects.some((project) => project.key === selected)
        "
        :value="selected"
        ><FolderGit2 aria-hidden="true" /><span class="truncate">{{
          selectedLabel
        }}</span></SelectItem
      >
      <SelectGroup v-if="projects.length">
        <SelectLabel>Projects on {{ host }}</SelectLabel>
        <SelectItem
          v-for="project in projects"
          :key="project.key"
          :value="project.key"
          ><FolderGit2 aria-hidden="true" /><span class="min-w-0"
            ><span class="block truncate">{{ project.name }}</span
            ><span class="block truncate text-xs text-muted-foreground">{{
              project.path
            }}</span></span
          ></SelectItem
        >
      </SelectGroup>
      <SelectSeparator />
      <SelectItem value="@add" :disabled="!agent"
        ><FolderOpen aria-hidden="true" /><span>Add project…</span></SelectItem
      >
      <SelectItem value="@new" :disabled="!agent"
        ><FolderPlus aria-hidden="true" /><span>New project…</span></SelectItem
      >
    </SelectContent>
  </Select>
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
