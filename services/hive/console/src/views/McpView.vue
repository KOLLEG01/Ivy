<script setup lang="ts">
import { PageHeader } from '@ivy/ui';
import { localeText as tr } from '../../../../../packages/ui-client/src/runtime';
import OpenaiPluginSettings from '../OpenaiPluginSettings.vue';
</script>

<template>
  <PageHeader
    title="MCP"
    :description="tr('Richte die MCP-Verbindung für diesen Hive ein.', 'Configure the MCP connection for this Hive.')"
  />
  <OpenaiPluginSettings />
</template>
