<script setup lang="ts">
import { PageHeader, SettingsSection } from "@ivy/ui";
import AgentEnvironment from "./AgentEnvironment.vue";
import AgentInstructions from "./AgentInstructions.vue";
import AgentDefaults from './AgentDefaults.vue';
import { tr } from './runtime';

defineProps<{ node: string }>();
</script>

<template>
  <div class="mx-auto w-full max-w-4xl px-4 py-6 md:px-8">
    <PageHeader title="Hive settings" />
    <SettingsSection :title="tr('Defaults je Servertyp', 'Defaults by server type')" :description="tr('Getrennte Vorgaben für Codex und Claude.', 'Separate defaults for Codex and Claude.')"><AgentDefaults /></SettingsSection>
    <SettingsSection
      title="Agent instructions"
      description="AgentManager uses the packaged template until a Hive AGENTS.md override is enabled here."
    >
      <AgentInstructions :host-id="null" />
    </SettingsSection>
    <SettingsSection
      title="MCP and skills"
      description="Shared defaults for every connected AgentManager. Secrets are never stored here."
    >
      <AgentEnvironment
        v-if="node"
        :node="node"
        :host-id="null"
      />
      <p v-else class="text-sm text-muted-foreground">
        Connect an AgentManager to inspect or override the packaged MCP and
        skill defaults.
      </p>
    </SettingsSection>
  </div>
</template>
