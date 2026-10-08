<script setup lang="ts">
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "@ivy/ui";
import type { Agent, RpcClient } from "../../sdk/src/client.js";
import ProjectLocationPicker from "./ProjectLocationPicker.vue";

const open = defineModel<boolean>("open", { default: false });
defineProps<{
  client: RpcClient;
  node: string;
  scope: string;
  host?: string | undefined;
  defaults?: Agent.ProjectsResult["defaults"];
  disabled?: boolean;
  kind: "normal" | "existing";
}>();
const emit = defineEmits<{ selected: [Agent.ProjectLocation] }>();
const select = (location: Agent.ProjectLocation) => {
  open.value = false;
  emit("selected", location);
};
</script>

<template>
  <Dialog v-model:open="open">
    <DialogContent class="max-h-[90dvh] overflow-y-auto sm:max-w-lg">
      <DialogHeader>
        <DialogTitle>Add project</DialogTitle>
        <DialogDescription
          >Choose a working location on {{ host ?? node }}.</DialogDescription
        >
      </DialogHeader>
      <ProjectLocationPicker
        v-if="open"
        :client="client"
        :node="node"
        :scope="scope"
        :host="host"
        :defaults="defaults"
        :disabled="disabled"
        :initial-kind="kind"
        @selected="select"
      />
    </DialogContent>
  </Dialog>
</template>
