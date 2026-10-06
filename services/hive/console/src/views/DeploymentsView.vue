<script setup lang="ts">
import { ref } from 'vue';
import { Disclosure, Button, ContentView, Input, Label, PageControls, PageHeader, RemoteState, StatusBadge } from '@ivy/ui';
import { consoleClient, dateLabel, navigate, usePage } from '../runtime';
const props = defineProps<{ query: URLSearchParams }>();
const host = ref(props.query.get('host') ?? ''), instance = ref(props.query.get('instance') ?? '');
const page = usePage((signal, cursor) => consoleClient.request('deployments.list', { limit: 50,
  ...(props.query.get('host') ? { hostId: props.query.get('host')! } : {}), ...(props.query.get('instance') ? { instanceId: props.query.get('instance')! } : {}), ...(cursor ? { cursor } : {}) }, { signal }), undefined, ["system"]);
</script>
<template>
  <PageHeader title="Deployments" description="Host operations ordered by their actual update time. Report time only shows when Hive last received the retained record; the host's durable journal remains authoritative." :loading="page.loading.value" :updated-at="page.updatedAt.value" />
  <form class="mb-6 flex flex-wrap items-end gap-3" @submit.prevent="navigate('deployments', { host, instance })"><div class="w-64"><Label for="deployment-host">Host ID</Label><Input id="deployment-host" v-model="host" class="mt-2" placeholder="All hosts" /></div><div class="w-64"><Label for="deployment-instance">Instance ID</Label><Input id="deployment-instance" v-model="instance" class="mt-2" placeholder="All instances" /></div><Button type="submit" variant="outline">Apply filters</Button></form>
  <RemoteState :loading="page.loading.value" :error="page.error.value" :has-data="!!page.value.value" :empty="page.value.value?.items.length === 0" empty-title="No deployment reports" empty-detail="Host managers publish their retained operations here when connected." @retry="page.refresh" />
  <Disclosure v-for="item in page.value.value?.items ?? []" :key="item.record.deploymentId" class="mb-3" variant="card"><template #trigger><span class="font-medium">{{ item.record.instanceId }}</span><span class="ml-3 text-xs text-muted-foreground">{{ item.record.hostId }}</span><StatusBadge class="ml-3" :label="item.record.phase" :tone="item.record.phase === 'succeeded' ? 'good' : item.record.phase === 'failed' || item.record.phase === 'needs_attention' ? 'bad' : 'neutral'" /><StatusBadge v-if="item.stale" class="ml-2" label="Stale report" tone="warning" /></template><p class="mt-3 break-all font-mono text-xs">{{ item.record.deploymentId }}</p><p class="mt-2 text-xs text-muted-foreground">Operation updated {{ dateLabel(item.record.updatedAt) }} · reported {{ dateLabel(item.reportedAt) }} by {{ item.serviceNodeId }}</p><ContentView class="mt-4" :text="JSON.stringify(item.record, null, 2)" media-type="application/json" /></Disclosure>
  <PageControls v-if="page.value.value" :count="page.value.value.items.length" :page="page.page.value" :has-next="!!page.value.value.nextCursor" :loading="page.loading.value" @next="page.next" @previous="page.previous" />
</template>
