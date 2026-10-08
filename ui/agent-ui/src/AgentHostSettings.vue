<script setup lang="ts">
import { ref, watch } from "vue";
import {
  Button,
  PageHeader,
  PropertyItem,
  PropertyList,
  RemoteState,
  SettingsSection,
  Textarea,
  useRemote,
} from "@ivy/ui";
import type { Agent } from "../../../packages/sdk/src/client.js";
import {
  newOperationId,
  serviceTools,
} from "../../../packages/sdk/src/client.js";
import { nativeRead } from "../../../packages/ui-client/src/native";
import { route } from "../../../packages/ui-client/src/runtime";
import AgentEnvironment from "./AgentEnvironment.vue";
import AgentInstructions from "./AgentInstructions.vue";
import { client } from "./runtime";

const props = defineProps<{ node: string }>();
const status = useRemote(
  async (signal) =>
    (await nativeRead(
      client,
      props.node,
      "agent.status",
      {},
      signal,
    )) as Agent.Status,
  15000,
);
const capabilityProfile = useRemote(
  async (signal) =>
    (await nativeRead(
      client,
      props.node,
      "agent.capabilities",
      {},
      signal,
    )) as Agent.CapabilityProfile,
);
const capabilityText = ref("");
const capabilityDirty = ref(false);
const capabilitySaving = ref(false);
const capabilityError = ref("");

watch(
  () => capabilityProfile.value.value,
  (value) => {
    if (value && !capabilityDirty.value)
      capabilityText.value = value.capabilities
        .map((item) => `${item.key} = ${item.label}`)
        .join("\n");
  },
  { immediate: true },
);

const saveCapabilities = async () => {
  const current = capabilityProfile.value.value;
  if (!current || capabilitySaving.value) return;
  capabilitySaving.value = true;
  capabilityError.value = "";
  try {
    const capabilities = capabilityText.value
      .split("\n")
      .map((value) => value.trim())
      .filter(Boolean)
      .map((value) => {
        const split = value.indexOf("=");
        return {
          key: (split < 0 ? value : value.slice(0, split)).trim(),
          label: (split < 0 ? value : value.slice(split + 1)).trim(),
        };
      });
    const operationId = await newOperationId(client);
    await serviceTools(client, props.node, [
      { namespace: "agent", interfaceVersion: "1.0.0" },
    ]).call(
      "agent.configureCapabilities",
      { operationId, expectedRevision: current.revision, capabilities },
      operationId,
    );
    capabilityDirty.value = false;
    await capabilityProfile.refresh();
  } catch (cause) {
    capabilityError.value =
      cause instanceof Error
        ? cause.message
        : "Capabilities could not be saved.";
  } finally {
    capabilitySaving.value = false;
  }
};

</script>

<template>
  <div class="mx-auto w-full max-w-4xl px-4 py-6 md:px-8">
    <PageHeader
      :title="`${status.value.value?.hostId ?? node} settings`"
      :loading="status.loading.value || capabilityProfile.loading.value"
      :updated-at="status.updatedAt.value"
      ><Button variant="ghost" size="sm" as-child
        ><a :href="route('settings')">Hive settings</a></Button
      ></PageHeader
    >

    <RemoteState
      :loading="status.loading.value"
      :error="status.error.value"
      :has-data="!!status.value.value"
      @retry="status.refresh"
    />

    <div v-if="status.value.value">
      <SettingsSection
        title="Connection"
        description="The native server this AgentManager talks to."
      >
        <PropertyList v-if="status.value.value.connection">
          <PropertyItem label="Server"
            >{{ status.value.value.connection.actualVersion }} ·
            {{ status.value.value.connection.mode }}</PropertyItem
          >
          <PropertyItem label="Process">{{
            status.value.value.connection.ownsServer
              ? "This AgentManager"
              : "Shared user daemon / external server"
          }}</PropertyItem>
          <PropertyItem label="Codex home"
            ><span class="break-all">{{
              status.value.value.connection.actualHome
            }}</span></PropertyItem
          >
          <PropertyItem label="MCP identity">{{
            status.value.value.connection.mcpIdentity
          }}</PropertyItem>
        </PropertyList>
        <p v-else class="text-sm text-muted-foreground">
          Connection details are unavailable for this provider.
        </p>
      </SettingsSection>

      <SettingsSection
        title="Scheduling capabilities"
        description="One stable key and label per line. These tags affect matching only."
      >
        <RemoteState :loading="capabilityProfile.loading.value" :error="capabilityProfile.error.value" :has-data="!!capabilityProfile.value.value" @retry="capabilityProfile.refresh" />
        <Textarea
          v-model="capabilityText"
          aria-label="Scheduling capabilities"
          class="min-h-32"
          placeholder="video-editing = Video editing"
          @input="capabilityDirty = true"
        />
        <div class="mt-3 flex items-center gap-3">
          <Button
            :disabled="
              !capabilityDirty ||
              capabilitySaving ||
              !capabilityProfile.value.value
            "
            @click="saveCapabilities"
            >Save capabilities</Button
          >
          <p
            v-if="capabilityError"
            role="alert"
            class="text-sm text-destructive"
          >
            {{ capabilityError }}
          </p>
        </div>
      </SettingsSection>

      <SettingsSection title="Defaults by server type" description="Model, working mode, reasoning effort and safety for new tasks.">
        <p class="mb-3 text-sm">Server type: {{ status.value.value.serverType === 'claude' ? 'Claude' : 'Codex' }}</p>
        <Button variant="outline" as-child><a :href="route('settings')">Configure server defaults</a></Button>
      </SettingsSection>

      <SettingsSection
        title="Agent instructions"
        description="This host inherits the Hive or packaged instructions unless a host-specific document is enabled."
      >
        <AgentInstructions
          :host-id="status.value.value.hostId"
          :status="status.value.value.instructions"
        />
      </SettingsSection>

      <SettingsSection
        title="MCP and skills"
        description="Secrets are never stored in these documents."
      >
        <AgentEnvironment
          :node="node"
          :host-id="status.value.value.hostId"
          :status="status.value.value.environment"
        />
      </SettingsSection>
    </div>
  </div>
</template>
