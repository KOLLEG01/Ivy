<script setup lang="ts">
import { ref } from 'vue';
import { Disclosure, Button, ContentView, Input, Label, PageControls, PageHeader, RemoteState, StatusBadge, Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from '@ivy/ui';
import { consoleClient, dateLabel, navigate, shortHash, usePage } from '../runtime';
const props = defineProps<{ query: URLSearchParams }>();
const host = ref(props.query.get('host') ?? '');
const page = usePage((signal, cursor) => consoleClient.request('serviceNodes.list', { limit: 50, ...(props.query.get('host') ? { hostId: props.query.get('host')! } : {}), ...(cursor ? { cursor } : {}) }, { signal }), undefined, ["services"]);
</script>
<template>
  <PageHeader title="Hosts & services" description="Actual service identities, software versions and connection observations. Disabled services are shown separately from failures." :loading="page.loading.value" :updated-at="page.updatedAt.value" />
  <form class="mb-6 flex flex-wrap items-end gap-3" @submit.prevent="navigate('nodes', { host })"><div class="w-72"><Label for="host-filter">Host ID</Label><Input id="host-filter" v-model="host" class="mt-2" placeholder="All hosts" /></div><Button variant="outline" type="submit">Apply filter</Button></form>
  <RemoteState :loading="page.loading.value" :error="page.error.value" :has-data="!!page.value.value" :empty="page.value.value?.items.length === 0" empty-title="No services found" empty-detail="Service Nodes appear after registration or a host management observation." @retry="page.refresh" />
  <div v-if="page.value.value?.items.length" class="overflow-hidden rounded-xl border bg-card"><Table><TableHeader><TableRow><TableHead>Service / host</TableHead><TableHead>State</TableHead><TableHead>Versions</TableHead><TableHead>Last contact</TableHead><TableHead><span class="sr-only">Details</span></TableHead></TableRow></TableHeader><TableBody>
    <TableRow v-for="node in page.value.value.items" :key="node.serviceNodeId"><TableCell><p class="font-medium">{{ node.serviceName }}</p><p class="mt-1 font-mono text-xs text-muted-foreground">{{ node.serviceNodeId }}</p><p class="mt-1 text-xs text-muted-foreground">{{ node.hostId }}</p></TableCell>
      <TableCell><StatusBadge :label="!node.desiredEnabled ? 'Disabled' : node.ready ? 'Reported ready' : !node.connected ? 'Disconnected' : !node.synced ? 'Syncing' : 'Not ready'" :tone="!node.desiredEnabled ? 'neutral' : node.ready ? 'good' : 'warning'" /><p v-if="node.connected && node.stale" class="mt-2 text-xs text-muted-foreground">Heartbeat was stale at the last refresh.</p><p class="mt-2 text-xs text-muted-foreground">Protocol {{ node.hiveProtocol }}</p></TableCell>
      <TableCell><p>{{ node.version }}</p><p class="mt-1 font-mono text-xs text-muted-foreground" :title="node.buildId">{{ shortHash(node.buildId) }}</p><p v-if="node.nativeVersion" class="mt-1 text-xs">Native {{ node.nativeVersion }}</p></TableCell><TableCell class="text-xs">{{ dateLabel(node.lastContactAt) }}<p class="mt-1 text-muted-foreground">Sync: {{ dateLabel(node.lastSuccessfulSyncAt) }}</p></TableCell>
      <TableCell><Disclosure title="Inspect"><ContentView class="mt-3 min-w-72" :text="JSON.stringify(node, null, 2)" /><Button variant="link" size="sm" @click="navigate('definitions', { node: node.serviceNodeId })">View definitions</Button></Disclosure></TableCell>
    </TableRow></TableBody></Table></div>
  <PageControls v-if="page.value.value" :count="page.value.value.items.length" :page="page.page.value" :has-next="!!page.value.value.nextCursor" :loading="page.loading.value" @next="page.next" @previous="page.previous" />
</template>
