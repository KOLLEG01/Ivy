<script setup lang="ts">
import { computed, onBeforeUnmount, ref } from 'vue';
import { Server } from '@lucide/vue';
import { Alert, AlertDescription, Button, IvyShell, uiIcon, useRemote } from '@ivy/ui';
import { uiUrl, consoleBase, consoleClient } from './runtime';
import { useBrowserApp } from '../../../../packages/ui-client/src/browser-app';
import UIsView from './views/UIsView.vue';
import NodesView from './views/NodesView.vue';
import DefinitionsView from './views/DefinitionsView.vue';
import DeploymentsView from './views/DeploymentsView.vue';
import ProblemsView from './views/ProblemsView.vue';
import ObjectsView from './views/ObjectsView.vue';
import RetentionView from './views/RetentionView.vue';
import ConfigurationsView from './views/ConfigurationsView.vue';
import ReleasesView from './views/ReleasesView.vue';
import McpDiscoveryView from './views/McpDiscoveryView.vue';

const hash = ref(location.hash || '#/uis'), changed = () => { hash.value = location.hash || '#/uis'; };
window.addEventListener('hashchange', changed); onBeforeUnmount(() => window.removeEventListener('hashchange', changed));
const section = computed(() => hash.value.slice(2).split('?')[0] || 'uis');
const query = computed(() => new URLSearchParams(hash.value.split('?')[1] ?? ''));
const systemPage = computed(() => section.value.startsWith('system/') ? section.value.slice('system/'.length) : '');
const systemPages = { nodes: NodesView, problems: ProblemsView, releases: ReleasesView, deployments: DeploymentsView, retention: RetentionView, definitions: DefinitionsView, objects: ObjectsView, settings: ConfigurationsView, mcp: McpDiscoveryView };
const view = computed(() => section.value === 'uis' ? UIsView : Object.hasOwn(systemPages, systemPage.value) ? systemPages[systemPage.value as keyof typeof systemPages] : null);
const status = useRemote(signal => consoleClient.request('system.status', {}, { signal }), 30000, ['system/runtime']);
const uis = useRemote(signal => consoleClient.request('uis.catalog', { limit: 50 }, { signal }), 60000, ['uis']);
const uiLinks = computed(() => (uis.value.value?.items ?? []).filter(ui => ui.currentReleaseId && ui.metadata.uiId !== 'chat-ui').map(ui => ({ id: ui.metadata.uiId, name: ui.metadata.displayName, href: uiUrl(ui), icon: uiIcon(ui.metadata.iconKey) })));
const systemNavigation = [
  { id: 'nodes', label: 'Hosts & services', group: 'Overview' }, { id: 'problems', label: 'Problems', group: 'Overview' },
  { id: 'releases', label: 'Releases', group: 'Runtime' }, { id: 'deployments', label: 'Deployments', group: 'Runtime' }, { id: 'retention', label: 'Retention', group: 'Runtime' },
  { id: 'definitions', label: 'Definitions', group: 'Data' }, { id: 'objects', label: 'Object Browser', group: 'Data' },
  { id: 'mcp', label: 'MCP Tools', group: 'Configuration' },
] as const;
// Home lists UIs and one entry into System; System has its own navigation.
const navigation = computed(() => systemPage.value
  ? systemNavigation.map(item => ({ label: item.label, href: '#/system/' + item.id, active: systemPage.value === item.id, group: item.group }))
  : [
    ...uiLinks.value.map(ui => ({ label: ui.name, href: ui.href, active: false, group: 'UIs', icon: ui.icon })),
    { label: 'System', href: '#/system/nodes', active: false, group: 'Administration', icon: Server },
  ]);
const logoutUrl = new URL('logout', consoleBase).href;
const browserApp = useBrowserApp(consoleBase);
const loginUrl = computed(() => new URL('login?returnTo=' + encodeURIComponent(consoleBase.pathname + hash.value), consoleBase).href);
</script>
<template>
  <IvyShell :ui-name="systemPage ? 'System' : 'Ivy'" :home-url="consoleBase.href" :apps="uiLinks" :current-app="systemPage ? 'system' : section === 'uis' ? '' : undefined" system-href="#/system/nodes" settings-href="#/system/settings" :logout-url="logoutUrl"
    :navigation="navigation" :browser-app="browserApp"
    :connection="status.error.value ? 'Connection unavailable' : status.value.value?.ready ? 'Hive ready' : 'Connecting'"
    :connection-tone="status.error.value ? 'warning' : status.value.value?.ready ? 'good' : 'neutral'">
    <Alert v-if="status.error.value" variant="destructive" class="mb-6">
      <AlertDescription class="flex flex-wrap items-center justify-between gap-3"><span>{{ status.error.value }}</span><Button variant="outline" size="sm" as-child><a :href="loginUrl">Sign in</a></Button></AlertDescription>
    </Alert>
    <component :is="view" v-if="view" :key="systemPage === 'objects' ? systemPage : hash" :query="query" />
    <div v-else><h1 class="text-2xl font-semibold">Page not found</h1><p class="mt-3 text-muted-foreground">Choose a view from the navigation.</p></div>
  </IvyShell>
</template>
