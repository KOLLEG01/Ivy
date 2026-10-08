<script setup lang="ts">
import { ref } from "vue";
import {
  Button,
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
  Label,
  OptionSelect,
  RemoteState,
  useRemote,
} from "@ivy/ui";
import type { Agent, BoundTool, RpcClient } from "../../sdk/src/client.js";
import { nativeRead } from "./native";
import { useNativeAction } from "./native-action";
import NativeActionState from "./NativeActionState.vue";
import { localeText as tr } from "./runtime";

const open = defineModel<boolean>("open", { default: false });
const props = defineProps<{
  client: RpcClient;
  node: string;
  threadId: string;
  projectId: string;
  binding: BoundTool;
  scope: string;
}>();
const emit = defineEmits<{ changed: [] }>();
const project = ref(props.projectId);
const projects = useRemote(
  async (signal) =>
    (await nativeRead(
      props.client,
      props.node,
      "agent.projects",
      {},
      signal,
    )) as Agent.ProjectsResult,
);
const action = useNativeAction(
  props.client,
  "ivy:task-project:" + props.scope + props.node + ":" + props.threadId,
);
const completed = () => {
  if (action.saved.value?.phase === "succeeded") {
    open.value = false;
    emit("changed");
  }
};
const save = async () => {
  if (
    action.locked.value ||
    projects.loading.value ||
    projects.error.value ||
    !projects.value.value
  )
    return;
  if (
    project.value &&
    !projects.value.value.projects.some(
      (value) => value.nativeId === project.value,
    )
  )
    return;
  await action.start("Change task project", props.binding, {
    threadId: props.threadId,
    projectId: project.value,
  });
  completed();
};
const reconcile = async () => {
  await action.reconcile();
  completed();
};
const retry = async () => {
  await action.retry();
  completed();
};
</script>

<template>
  <Dialog v-model:open="open">
    <DialogContent class="sm:max-w-lg">
      <DialogHeader>
        <DialogTitle>{{ tr("Projekt ändern", "Change project") }}</DialogTitle>
        <DialogDescription>{{
          tr(
            "Ordne diese Task einem Projekt zu.",
            "Assign this task to a project.",
          )
        }}</DialogDescription>
      </DialogHeader>
      <RemoteState
        :loading="projects.loading.value"
        :error="projects.error.value"
        :has-data="!!projects.value.value"
        @retry="projects.refresh"
      />
      <Label for="task-project">{{ tr("Projekt", "Project") }}</Label>
      <OptionSelect
        id="task-project"
        v-model="project"
        class="w-full"
        :disabled="
          action.locked.value ||
          projects.loading.value ||
          !!projects.error.value
        "
      >
        <option value="">{{ tr("Ohne Projekt", "No project") }}</option>
        <option
          v-if="
            project &&
            !projects.value.value?.projects.some(
              (value) => value.nativeId === project,
            )
          "
          :value="project"
          disabled
        >
          {{ tr("Projekt nicht verfügbar", "Project unavailable") }}
        </option>
        <option
          v-for="item in projects.value.value?.projects ?? []"
          :key="item.nativeId"
          :value="item.nativeId"
        >
          {{ item.name }}
        </option>
      </OptionSelect>
      <NativeActionState
        :action="action.saved.value"
        :busy="action.busy.value"
        :error="action.error.value"
        @reconcile="reconcile"
        @retry="retry"
      />
      <DialogFooter>
        <Button variant="outline" @click="open = false">{{
          tr("Abbrechen", "Cancel")
        }}</Button>
        <Button
          :loading="action.pending.value"
          :disabled="
            action.locked.value ||
            projects.loading.value ||
            !!projects.error.value ||
            !projects.value.value ||
            (!!project &&
              !projects.value.value.projects.some(
                (value) => value.nativeId === project,
              ))
          "
          @click="save"
        >
          {{ tr("Speichern", "Save") }}
        </Button>
      </DialogFooter>
    </DialogContent>
  </Dialog>
</template>
