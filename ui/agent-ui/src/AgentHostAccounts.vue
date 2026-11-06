<script setup lang="ts">
import { computed, ref, watch } from 'vue';
import { Collapsible, CollapsibleContent, CollapsibleTrigger, Label, OptionSelect, RemoteState, useRemote } from '@ivy/ui';
import type { Agent } from '../../../packages/sdk/src/client.js';
import { nativeBackendLabel, nativeBackendScopes, sameBackend } from '../../../packages/ui-client/src/native-backends';
import { nativeOwnerKey, nativeRouteQuery } from '../../../packages/ui-client/src/native-route';
import { serviceNodeLabel } from '../../../packages/ui-client/src/native';
import type { NativeHostNode } from '../../../packages/ui-client/src/native-hosts';
import { route } from '../../../packages/ui-client/src/runtime';
import { base, client, tr } from './runtime';
import AgentHost from './AgentHost.vue';
import AgentHistory from './AgentHistory.vue';
const props = defineProps<{ node: string; nodes: NativeHostNode[]; backend?: Agent.BackendIdentity | undefined; project: string; projectId: string }>();
const discovery = useRemote(async signal => {
  const hostId = props.nodes.find(node => node.serviceNodeId === props.node)?.hostId;
  const own = props.nodes.filter(node => node.hostId === hostId && node.connected);
  const results = await Promise.all(own.map(async node => {
    try { return { scopes: await nativeBackendScopes(client, node, signal), error: '' }; }
    catch (error) { signal.throwIfAborted(); return { scopes: [], error: `${node.serviceNodeId}: ${error instanceof Error ? error.message : String(error)}` }; }
  }));
  return { scopes: results.flatMap(result => result.scopes), errors: results.map(result => result.error).filter(Boolean) };
}, 30000, ['services/agent-manager']);
watch(() => props.nodes.find(node => node.serviceNodeId === props.node)?.hostId, () => void discovery.refresh());
const choices = computed(() => discovery.value.value?.scopes ?? []);
const selected = computed(() => choices.value.find(scope => scope.serviceNodeId === props.node && sameBackend(scope.backend, props.backend)));
const historyOpen = ref(false);
const selection = computed(() => selected.value ? nativeOwnerKey({ ...selected.value, namespace: selected.value.backend?.backendId ?? 'codex' }) : '');
const choose = (value: string) => {
  const scope = choices.value.find(scope => nativeOwnerKey({ ...scope, namespace: scope.backend?.backendId ?? 'codex' }) === value);
  if (scope) location.hash = route('host', nativeRouteQuery({ ...scope, namespace: scope.backend?.backendId ?? 'codex' }));
};
// Keep an unfinished legacy launch at its original route; selecting an account never adopts it.
const legacyRetained = ref(false);
if (!props.backend) try {
  legacyRetained.value = !!sessionStorage.getItem('ivy:agent-launch:' + base.href + props.node) ||
    ['ivy:agent-create:', 'ivy:agent-first-message:'].some(prefix => {
      const saved = JSON.parse(sessionStorage.getItem(prefix + base.href + props.node) ?? 'null');
      return saved && !['succeeded', 'failed'].includes(saved.phase);
    });
} catch { legacyRetained.value = true; }
</script>

<template>
  <div class="mx-auto w-full max-w-3xl px-4 pt-4">
    <Label for="agent-account">Provider and account</Label>
    <OptionSelect id="agent-account" :model-value="selection" class="mt-2 w-full" @update:model-value="choose">
      <option value="" disabled>{{ backend ? 'Original account unavailable' : 'Choose an account' }}</option>
      <option v-for="scope in choices" :key="nativeOwnerKey({ ...scope, namespace: scope.backend?.backendId ?? 'codex' })"
        :value="nativeOwnerKey({ ...scope, namespace: scope.backend?.backendId ?? 'codex' })">
        {{ scope.backend ? nativeBackendLabel(scope.backend) : serviceNodeLabel(nodes, scope.serviceNodeId) + ' · ' + tr('Bestehender Account', 'Existing account') }}{{ scope.status.state === 'ready' ? '' : ' · ' + scope.status.state }}
      </option>
    </OptionSelect>
    <RemoteState :loading="discovery.loading.value" :error="discovery.error.value" :has-data="!!discovery.value.value" @retry="discovery.refresh" />
    <p v-for="error in discovery.value.value?.errors" :key="error" class="mt-2 text-sm text-muted-foreground">{{ error }}</p>
    <p v-if="!backend && legacyRetained" class="mt-2 text-sm text-muted-foreground">The unfinished launch keeps its original unscoped owner. It cannot be assigned to another account.</p>
  </div>
  <Collapsible v-if="selected" :key="selection" v-model:open="historyOpen" class="mx-auto w-full max-w-3xl px-4 pt-6">
    <CollapsibleTrigger class="text-sm font-medium underline underline-offset-4">{{ tr('Aufgabenverlauf ansehen', 'Browse task history') }}</CollapsibleTrigger>
    <CollapsibleContent class="pt-4"><AgentHistory v-if="historyOpen" :key="selection" :node="node" :backend="backend" /></CollapsibleContent>
  </Collapsible>
  <AgentHost v-if="selected || !backend && legacyRetained" :key="selection || node" :node="node" :backend="backend" :project="project" :project-id="projectId" />
</template>
