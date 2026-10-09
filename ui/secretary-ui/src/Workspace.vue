<script setup lang="ts">
import { computed } from "vue";
import {
  OptionSelect,
  RemoteState,
  SidebarGroup,
  SidebarGroupContent,
  SidebarGroupLabel,
  useRemote,
} from "@ivy/ui";
import { ListTodo, ScrollText } from "@lucide/vue";
import UiFrame from "../../../packages/ui-client/src/UiFrame.vue";
import type { Operation } from "../../../packages/sdk/src/client.js";
import { base, client, loadWorkspace, route, tr } from "./runtime";
import JournalView from "./JournalView.vue";
import AssignmentsView from "./AssignmentsView.vue";
import RulesView from "./RulesView.vue";
const props = defineProps<{
  node: string;
  services: Operation.ServiceNode[];
  section: string;
}>();
const emit = defineEmits<{ selectNode: [value: string] }>();
const workspace = useRemote((signal) => loadWorkspace(props.node, signal), 30000, () => ["services/secretary/" + props.node, "objects/secretary/configuration"]);
const navigation = computed(() => [
  {
    label: tr("Journal", "Journal"),
    href: route("journal"),
    active: !["assignments", "rules"].includes(props.section),
    icon: ScrollText,
  },
  {
    label: tr("Aufträge", "Assignments"),
    href: route("assignments"),
    active: props.section === "assignments",
    icon: ListTodo,
  },
]);
</script>
<template>
  <UiFrame
    ui-name="Secretary"
    :base="base"
    :client="client"
    :navigation="navigation"
    :settings-href="route('rules')"
    :settings-label="tr('Einstellungen', 'Settings')"
  >
    <template v-if="services.length > 1" #sidebar>
      <SidebarGroup>
        <SidebarGroupLabel>{{
          tr("Secretary-Instanz", "Secretary instance")
        }}</SidebarGroupLabel>
        <SidebarGroupContent class="px-2">
          <OptionSelect
            :model-value="node"
            class="w-full"
            :aria-label="tr('Secretary-Instanz', 'Secretary instance')"
            @update:model-value="emit('selectNode', String($event))"
          >
            <option
              v-for="service in services"
              :key="service.serviceNodeId"
              :value="service.serviceNodeId"
            >
              {{ service.serviceNodeId
              }}{{ service.ready && service.connected ? "" : " · offline" }}
            </option>
          </OptionSelect>
        </SidebarGroupContent>
      </SidebarGroup>
    </template>
    <RemoteState
      :loading="workspace.loading.value"
      :error="workspace.error.value"
      :has-data="!!workspace.value.value"
      @retry="workspace.refresh"
    />
    <template v-if="workspace.value.value">
      <AssignmentsView
        v-if="section === 'assignments'"
        :node="node"
        :root="workspace.value.value.scope.rootObjectId"
      />
      <RulesView
        v-else-if="section === 'rules'"
        :node="node"
        :host-id="
          services.find((service) => service.serviceNodeId === node)?.hostId ??
          ''
        "
        :root="workspace.value.value.scope.rootObjectId"
        :configuration="workspace.value.value.configuration"
      />
      <JournalView v-else :root="workspace.value.value.scope.rootObjectId" />
    </template>
  </UiFrame>
</template>
