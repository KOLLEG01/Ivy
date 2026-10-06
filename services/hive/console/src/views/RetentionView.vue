<script setup lang="ts">
import { Card, CardContent, CardHeader, CardTitle, PageHeader, RemoteState, StatusBadge, useRemote } from '@ivy/ui';
import { consoleClient, dateLabel } from '../runtime';

const status = useRemote(signal => consoleClient.request('retention.status', {}, { signal }), 30_000);
const mode = (policy: { objects: { mode: string }; revisions: { mode: string; maximumCount?: number; maximumAgeDays?: number } }) => {
  const limits = [policy.revisions.maximumCount ? `newest ${policy.revisions.maximumCount}` : '', policy.revisions.maximumAgeDays ? `${policy.revisions.maximumAgeDays} days` : ''].filter(Boolean).join(' / ');
  return `${policy.objects.mode} · ${policy.revisions.mode}${limits ? ` (${limits})` : ''}`;
};
</script>
<template>
  <PageHeader title="Retention" description="Current object, revision, event and mutation-journal retention. Collection is automatic; a new family policy is previewed once before enforcement." :loading="status.loading.value" />
  <RemoteState :loading="status.loading.value" :error="status.error.value" :has-data="!!status.value.value" @retry="status.refresh" />
  <template v-if="status.value.value">
    <div class="mb-6 grid gap-3 md:grid-cols-3">
      <Card><CardHeader><CardTitle>Collector</CardTitle></CardHeader><CardContent><p class="text-sm">Last success: {{ dateLabel(status.value.value.lastSuccessAt) }}</p><p class="text-sm">Next pass: {{ dateLabel(status.value.value.nextPassAt) }}</p><p v-if="status.value.value.lastFailure" class="mt-2 text-sm text-destructive">{{ status.value.value.lastFailure.message }}</p></CardContent></Card>
      <Card><CardHeader><CardTitle>Events</CardTitle></CardHeader><CardContent><p class="text-sm">{{ status.value.value.events.count }} / 10,000 · {{ status.value.value.events.byteLength.toLocaleString() }} / 67,108,864 bytes</p><p class="text-xs text-muted-foreground">Pruned through {{ status.value.value.events.prunedThroughSequence }}</p></CardContent></Card>
      <Card><CardHeader><CardTitle>Mutation receipts</CardTitle></CardHeader><CardContent><p class="text-sm">{{ status.value.value.mutations.count }} / 10,000 · {{ status.value.value.mutations.byteLength.toLocaleString() }} / 268,435,456 bytes</p><p class="text-xs text-muted-foreground">Expired-before cutoff {{ new Date(status.value.value.mutations.expiredBefore).toLocaleString() }}</p></CardContent></Card>
    </div>
    <div class="space-y-3">
      <Card v-for="family in status.value.value.families" :key="family.contractKey"><CardHeader class="flex-row flex-wrap items-center justify-between gap-2"><CardTitle class="font-mono text-sm">{{ family.contractKey }}</CardTitle><StatusBadge :label="family.previewRequired ? 'Preview pending' : 'Active'" :tone="family.previewRequired ? 'warning' : 'good'" /></CardHeader><CardContent><p class="text-sm">{{ mode(family.policy) }}</p><p class="mt-2 text-xs text-muted-foreground">{{ family.objectCount }} objects · {{ family.revisionCount }} revisions · {{ family.byteLength.toLocaleString() }} bytes · eligible {{ family.eligibleObjectCount }} objects / {{ family.eligibleRevisionCount }} revisions · protected {{ family.protectedRevisionCount }}</p></CardContent></Card>
    </div>
  </template>
</template>
