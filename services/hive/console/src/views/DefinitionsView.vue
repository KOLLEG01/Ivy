<script setup lang="ts">
import { ref } from 'vue';
import { Disclosure, Button, ContentView, Input, Label, PageControls, PageHeader, RemoteState, StatusBadge } from '@ivy/ui';
import { consoleClient, dateLabel, navigate, usePage } from '../runtime';
import type { Operation } from '../runtime';
const props = defineProps<{ query: URLSearchParams }>();
const node = ref(props.query.get('node') ?? ''), namespace = ref(props.query.get('namespace') ?? ''), contract = ref(props.query.get('contract') ?? '');
const selected = !!props.query.get('node') && !!props.query.get('namespace');
const namespaces = usePage((signal, cursor) => consoleClient.request('namespaces.list', { limit: 50, ...(cursor ? { cursor } : {}) }, { signal }), undefined, ["services"]);
const tools = usePage(async (signal, cursor): Promise<Operation.ToolsListResult | { items: Operation.ToolBinding[]; nextCursor: null }> => selected
  ? consoleClient.request('tools.list', { serviceNodeId: props.query.get('node')!, namespace: props.query.get('namespace')!, limit: 50, ...(cursor ? { cursor } : {}) }, { signal })
  : { items: [], nextCursor: null }, undefined, ["services"]);
const contracts = usePage((signal, cursor) => consoleClient.request('contracts.list', { limit: 50, allVersions: true,
  ...(props.query.get('contract') ? { key: props.query.get('contract')! } : {}), ...(cursor ? { cursor } : {}) }, { signal }), undefined, ["services"]);
const supported = usePage(async (signal, cursor) => props.query.get('node')
  ? consoleClient.request('serviceNodes.contracts', { serviceNodeId: props.query.get('node')!, limit: 50, ...(cursor ? { cursor } : {}) }, { signal })
  : { items: [], nextCursor: null }, undefined, ["services"]);
const refresh = () => { void namespaces.refresh(); void tools.refresh(); void contracts.refresh(); void supported.refresh(); };
</script>
<template>
  <PageHeader title="Definitions" description="Inspect exact tool schemas and hashes on an explicitly selected Service Node. Data Contracts below show the definitions registered in this Hive." :loading="namespaces.loading.value || tools.loading.value || contracts.loading.value" />
  <form class="mb-6 flex flex-wrap items-end gap-3" @submit.prevent="navigate('definitions', { node, namespace, contract })"><div class="w-64"><Label for="definition-node">Service Node ID</Label><Input id="definition-node" v-model="node" class="mt-2" /></div><div class="w-52"><Label for="definition-namespace">Namespace</Label><Input id="definition-namespace" v-model="namespace" class="mt-2" /></div><Button type="submit" variant="outline">Inspect tools</Button></form>
  <section v-if="!selected" aria-label="Available namespaces"><RemoteState :loading="namespaces.loading.value" :error="namespaces.error.value" :has-data="!!namespaces.value.value" :empty="namespaces.value.value?.items.length === 0" empty-title="No namespaces registered" @retry="namespaces.refresh" />
    <div class="grid gap-3 md:grid-cols-2"><article v-for="item in namespaces.value.value?.items ?? []" :key="item.namespace" class="rounded-lg border bg-card p-4"><h2 class="font-semibold">{{ item.namespace }}</h2><div v-for="provider in item.providers.filter(value => !node || value.node.serviceNodeId === node)" :key="provider.node.serviceNodeId" class="mt-3 flex flex-wrap items-center justify-between gap-2 text-sm"><span class="break-all font-mono text-xs">{{ provider.node.serviceNodeId }}</span><Button variant="outline" size="sm" @click="navigate('definitions', { node: provider.node.serviceNodeId, namespace: item.namespace, contract })">{{ provider.eligible ? 'Inspect' : 'Inspect retained' }}</Button></div></article></div>
    <PageControls v-if="namespaces.value.value" :count="namespaces.value.value.items.length" :page="namespaces.page.value" :has-next="!!namespaces.value.value.nextCursor" :loading="namespaces.loading.value" @next="namespaces.next" @previous="namespaces.previous" />
  </section>
  <section v-else aria-label="Selected tool definitions"><RemoteState :loading="tools.loading.value" :error="tools.error.value" :has-data="!!tools.value.value" :empty="tools.value.value?.items.length === 0" empty-title="No matching tools" @retry="tools.refresh" />
    <div v-if="tools.value.value && 'provider' in tools.value.value" class="mb-4 flex flex-wrap items-center gap-3 text-sm"><StatusBadge :label="tools.value.value.provider.eligible ? 'Available' : 'Retained catalog'" :tone="tools.value.value.provider.eligible ? 'good' : 'warning'" /><span>{{ tools.value.value.provider.node.serviceName }} · {{ tools.value.value.provider.node.version }}</span><Disclosure class="w-full" title="Namespace guide"><ContentView class="mt-3" :text="tools.value.value.guideMarkdown" media-type="text/markdown" /></Disclosure></div>
    <Disclosure v-for="tool in tools.value.value?.items ?? []" :key="tool.qualifiedName" class="mb-3" variant="card"><template #trigger><span class="font-mono text-sm font-medium">{{ tool.qualifiedName }}</span><span class="ml-3 text-xs text-muted-foreground">{{ tool.definition.interfaceVersion }}</span></template><p class="mt-3 text-sm text-muted-foreground">{{ tool.definition.description }}</p><p class="my-3 break-all font-mono text-xs">{{ tool.definitionHash }}</p><ContentView :text="JSON.stringify(tool.definition, null, 2)" media-type="application/json" /></Disclosure>
    <PageControls v-if="tools.value.value" :count="tools.value.value.items.length" :page="tools.page.value" :has-next="!!tools.value.value.nextCursor" :loading="tools.loading.value" @next="tools.next" @previous="tools.previous" />
  </section>
  <section v-if="props.query.get('node')" class="mt-10 border-t pt-7" aria-labelledby="supported-heading">
    <h2 id="supported-heading" class="text-lg font-semibold">Supported Data Contracts on this node</h2>
    <RemoteState :loading="supported.loading.value" :error="supported.error.value" :has-data="!!supported.value.value" @retry="supported.refresh" />
    <div v-if="supported.value.value && 'provider' in supported.value.value" class="mt-4 space-y-3">
      <div class="flex flex-wrap items-center gap-3 text-sm"><StatusBadge :label="supported.value.value.provider.eligible ? 'Current provider' : 'Retained declarations'" :tone="supported.value.value.provider.eligible ? 'good' : 'warning'" /><span>Catalog captured {{ dateLabel(supported.value.value.capturedAt) }}</span></div>
      <p v-if="!supported.value.value.hasCatalog" class="text-sm text-muted-foreground">This node has not supplied a successful catalog yet.</p>
      <p v-else-if="supported.value.value.items.length === 0" class="text-sm text-muted-foreground">This catalog declares no Data Contract access.</p>
      <article v-for="(item, index) in supported.value.value.items" :key="index" class="rounded-lg border bg-card p-4 text-sm">
        <h3 class="font-semibold">{{ item.key }}</h3><p class="mt-2">Reads: {{ item.readVersions.join(', ') || 'None declared' }}</p><p>Writes: {{ item.writeVersions.join(', ') || 'None declared' }}</p>
      </article>
      <PageControls :count="supported.value.value.items.length" :page="supported.page.value" :has-next="!!supported.value.value.nextCursor" :loading="supported.loading.value" @next="supported.next" @previous="supported.previous" />
    </div>
  </section>
  <section class="mt-10 border-t pt-7" aria-labelledby="contracts-heading"><h2 id="contracts-heading" class="text-lg font-semibold">Registered Data Contracts</h2><form class="my-5 flex flex-wrap items-end gap-3" @submit.prevent="navigate('definitions', { node, namespace, contract })"><div class="w-72"><Label for="contract-filter">Exact contract key</Label><Input id="contract-filter" v-model="contract" class="mt-2" placeholder="All contracts" /></div><Button variant="outline" type="submit">Filter contracts</Button></form>
    <RemoteState :loading="contracts.loading.value" :error="contracts.error.value" :has-data="!!contracts.value.value" :empty="contracts.value.value?.items.length === 0" empty-title="No contracts found" @retry="contracts.refresh" />
    <Disclosure v-for="item in contracts.value.value?.items ?? []" :key="item.key + '@' + item.version" class="mb-3" variant="card"><template #trigger><strong>{{ item.key }}@{{ item.version }}</strong><span class="ml-3 text-muted-foreground">{{ item.mediaType }}</span></template><p class="mt-3 text-xs text-muted-foreground">Owner: {{ item.owner.kind }}{{ item.owner.kind === 'service' ? ' / ' + item.owner.serviceName : '' }} · Retention: {{ item.retention.objects.mode }}{{ item.retention.objects.mode === 'expire' ? ` (${item.retention.objects.maximumAgeDays} days)` : '' }} / {{ item.retention.revisions.mode }}</p><ContentView class="mt-4" :text="item.specMarkdown" media-type="text/markdown" /><ContentView v-if="item.jsonSchema !== undefined" class="mt-4" :text="JSON.stringify(item.jsonSchema, null, 2)" media-type="application/json" /></Disclosure>
    <PageControls v-if="contracts.value.value" :count="contracts.value.value.items.length" :page="contracts.page.value" :has-next="!!contracts.value.value.nextCursor" :loading="contracts.loading.value" @next="contracts.next" @previous="contracts.previous" />
  </section>
</template>
