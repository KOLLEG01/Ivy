<script setup lang="ts">
import { ref, watch } from 'vue';
import { Disclosure, Card, CardContent, CardHeader, ContentView, Label, OptionSelect, PageControls, PageHeader, RemoteState, StatusBadge } from '@ivy/ui';
import { consoleClient, dateLabel, navigate, usePage } from '../runtime';
const props = defineProps<{ query: URLSearchParams }>();
// Resolved findings are history; the default view lists only what still needs attention.
const states = ['current', 'stale', 'unknown', 'resolved', 'all'] as const;
const status = ref<string>(states.find(value => value === props.query.get('status')) ?? '');
const selected = status.value === 'all' ? undefined : (status.value || 'unresolved') as Exclude<typeof states[number], 'all'> | 'unresolved';
watch(status, value => navigate('problems', { status: value }));
const page = usePage((signal, cursor) => consoleClient.request('system.diagnostics', { limit: 50, ...(selected ? { status: selected } : {}), ...(cursor ? { cursor } : {}) }, { signal }), undefined, ["services", "system"]);
</script>
<template>
  <PageHeader title="Problems" description="Concrete diagnostics with their affected resource, severity and observation time. Current, stale, unknown and resolved findings remain distinct." :loading="page.loading.value" :updated-at="page.updatedAt.value" />
  <div class="mb-6 min-w-56 sm:max-w-64"><Label for="problem-state">Observation state</Label><OptionSelect id="problem-state" v-model="status" class="mt-2"><option value="">Unresolved</option><option v-for="state in states" :key="state" :value="state">{{ state === 'all' ? 'All states' : state }}</option></OptionSelect></div>
  <RemoteState :loading="page.loading.value" :error="page.error.value" :has-data="!!page.value.value" :empty="page.value.value?.items.length === 0" empty-title="No matching diagnostics" :empty-detail="(selected === 'unresolved' ? 'No unresolved diagnostics are recorded.' : 'No diagnostic records match this view.') + ' This does not replace checking the readiness of required hosts and services.'" @retry="page.refresh" />
  <Card v-for="(item, index) in page.value.value?.items ?? []" :key="item.code + ':' + index" class="mb-3"><CardHeader class="flex-row flex-wrap items-center gap-3"><h2 class="font-mono text-sm font-semibold">{{ item.code }}</h2><StatusBadge :label="item.severity" :tone="item.severity === 'error' ? 'bad' : item.severity === 'warning' ? 'warning' : 'neutral'" /><StatusBadge :label="item.status" :tone="item.status === 'resolved' ? 'good' : item.status === 'unknown' || item.status === 'stale' ? 'warning' : 'neutral'" /></CardHeader><CardContent><p class="text-sm leading-relaxed">{{ item.message }}</p><p class="mt-3 text-xs text-muted-foreground">{{ item.source }} · First {{ dateLabel(item.firstObservedAt) }} · Last {{ dateLabel(item.lastObservedAt) }}</p><Disclosure class="mt-3" title="Affected resource"><ContentView class="mt-2" :text="JSON.stringify(item.resource, null, 2)" /></Disclosure></CardContent></Card>
  <PageControls v-if="page.value.value" :count="page.value.value.items.length" :page="page.page.value" :has-next="!!page.value.value.nextCursor" :loading="page.loading.value" @next="page.next" @previous="page.previous" />
</template>
