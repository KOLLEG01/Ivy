<script setup lang="ts">
import { computed, ref, watch } from 'vue';
import { Disclosure, Button, ContentView, Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle, Label, OptionSelect, PageControls, PageHeader, RemoteState, StatusBadge, Table, TableBody, TableCell, TableHead, TableHeader, TableRow, useRemote } from '@ivy/ui';
import { IvyError, newOperationId } from '../../../../../packages/sdk/src/client';
import { consoleBase, consoleClient, dateLabel, shortHash, usePage } from '../runtime';
import type { Operation } from '../runtime';

const page = usePage((signal, cursor) => consoleClient.request('uis.catalog', { limit: 50, ...(cursor ? { cursor } : {}) }, { signal }), undefined, ["uis"]);
const packages = useRemote(signal => consoleClient.request('packages.catalog', { after: 0, includeUis: true }, { signal }), 0, ["objects/ivy/package-catalog"]);
const packageItems = computed(() => [...(packages.value.value?.catalog.packages ?? [])].filter(item => item.componentId !== 'chat-ui').sort((left, right) => right.revision - left.revision));
const statuses = { ready: 'Available', unavailable: 'Dependency unavailable', incompatible: 'Update required', invalid: 'Invalid release', missing: 'Missing release', unselected: 'No active release' };
const selected = ref<Operation.UiSummary | null>(null), releaseId = ref(''), pending = ref<Operation.UisRollbackParams | null>(null);
const busy = ref(false), feedback = ref<string | null>(null);
const releases = usePage(async (signal, cursor) => selected.value
  ? consoleClient.request('uis.releases', { uiId: selected.value.metadata.uiId, limit: 50, ...(cursor ? { cursor } : {}) }, { signal })
  : { items: [], nextCursor: null }, undefined, ["uis"]);
const inspection = useRemote(signal => selected.value
  ? consoleClient.request('uis.inspect', { uiId: selected.value.metadata.uiId, ...(releaseId.value ? { releaseId: releaseId.value } : {}) }, { signal })
  : Promise.resolve(null));
const inspected = computed(() => inspection.value.value?.requestedReleaseId === (releaseId.value || selected.value?.currentReleaseId) ? inspection.value.value : null);
const storageKey = (uiId: string) => 'ivy.console.pendingRollback:' + consoleBase.href + ':' + uiId;
const inspect = (ui: Operation.UiSummary) => {
  selected.value = ui; releaseId.value = ''; feedback.value = null; pending.value = null;
  try {
    const raw = sessionStorage.getItem(storageKey(ui.metadata.uiId));
    if (raw && raw.length < 8192) {
      const saved = JSON.parse(raw) as Partial<Operation.UisRollbackParams>;
      if (saved.uiId === ui.metadata.uiId && typeof saved.releaseId === 'string' && typeof saved.expectedReleaseId === 'string' && typeof saved.mutationId === 'string') {
        pending.value = { uiId: saved.uiId, releaseId: saved.releaseId, expectedReleaseId: saved.expectedReleaseId, mutationId: saved.mutationId };
        releaseId.value = saved.releaseId;
      }
    }
  } catch { feedback.value = 'The previous selection record could not be read.'; }
  releases.reset(); void inspection.refresh();
};
watch(releaseId, () => { void inspection.refresh(); });
const rollback = async () => {
  if (!selected.value?.currentReleaseId || !releaseId.value || busy.value) return;
  const request = pending.value ?? { uiId: selected.value.metadata.uiId, releaseId: releaseId.value, expectedReleaseId: selected.value.currentReleaseId, mutationId: await newOperationId(consoleClient) };
  busy.value = true; feedback.value = null;
  try {
    sessionStorage.setItem(storageKey(request.uiId), JSON.stringify(request)); pending.value = request;
    const result = await consoleClient.request('uis.rollback', request);
    pending.value = null; sessionStorage.removeItem(storageKey(request.uiId)); feedback.value = 'Selected release ' + result.releaseId + '.';
    selected.value.currentReleaseId = result.releaseId; await Promise.all([page.refresh(), inspection.refresh()]);
  } catch (error) {
    feedback.value = error instanceof Error ? error.message : 'The release selection could not be confirmed.';
    if (error instanceof IvyError && error.outcome === 'not_executed') {
      pending.value = null; sessionStorage.removeItem(storageKey(request.uiId)); feedback.value = 'Selection was refused: ' + error.message;
      await Promise.all([page.refresh(), inspection.refresh()]); if (inspection.value.value) selected.value.currentReleaseId = inspection.value.value.currentReleaseId;
    }
  } finally { busy.value = false; }
};
</script>
<template>
  <PageHeader title="Releases" description="Inspect immutable component packages and explicitly select a compatible retained web UI release." :loading="page.loading.value || packages.loading.value" :updated-at="page.updatedAt.value" />
  <section class="mb-10" aria-labelledby="package-releases-heading"><h2 id="package-releases-heading" class="text-lg font-semibold">Component packages</h2><p class="mt-1 text-sm text-muted-foreground">All validated packages in the central Hive catalog, newest first.</p>
    <RemoteState class="mt-4" :loading="packages.loading.value" :error="packages.error.value" :has-data="!!packages.value.value" :empty="packageItems.length === 0" empty-title="No packages published" @retry="packages.refresh" />
    <div v-if="packageItems.length" class="mt-4 overflow-hidden rounded-xl border bg-card"><Table><TableHeader><TableRow><TableHead>Component</TableHead><TableHead>Version</TableHead><TableHead>Published</TableHead><TableHead>Size</TableHead></TableRow></TableHeader><TableBody>
      <TableRow v-for="item in packageItems" :key="item.componentId + ':' + item.version"><TableCell><p class="font-medium">{{ item.componentId }}</p><p class="mt-1 text-xs text-muted-foreground">{{ item.manifest.kind }} · {{ shortHash(item.buildId) }}</p></TableCell><TableCell>{{ item.version }}</TableCell><TableCell class="text-sm">{{ dateLabel(item.publishedAt) }}</TableCell><TableCell class="text-sm">{{ item.bytes.toLocaleString() }} B</TableCell></TableRow>
    </TableBody></Table></div>
  </section>
  <section aria-labelledby="ui-releases-heading"><h2 id="ui-releases-heading" class="text-lg font-semibold">Web UI releases</h2><p class="mb-4 mt-1 text-sm text-muted-foreground">Current pointers, retained assets and compatibility checks.</p>
  <RemoteState :loading="page.loading.value" :error="page.error.value" :has-data="!!page.value.value" :empty="page.value.value?.items.filter(ui => ui.metadata.uiId !== 'chat-ui').length === 0" empty-title="No web UI releases" @retry="page.refresh" />
  <div class="grid gap-3">
    <article v-for="ui in page.value.value?.items.filter(value => value.metadata.uiId !== 'chat-ui') ?? []" :key="ui.metadata.uiId" class="flex flex-wrap items-center gap-4 rounded-xl border bg-card p-5">
      <div class="min-w-0 flex-1"><h2 class="font-semibold">{{ ui.metadata.displayName }}</h2><p class="mt-1 break-all text-sm text-muted-foreground">{{ ui.currentReleaseId ?? 'No active release' }} · {{ ui.releaseCount }} retained</p></div>
      <StatusBadge v-if="!ui.currentReleaseId" label="No active release" tone="warning" /><Button variant="outline" size="sm" @click="inspect(ui)">Manage releases</Button>
    </article>
  </div>
  <PageControls v-if="page.value.value && (page.value.value.nextCursor || page.page.value > 1)" :count="page.value.value.items.length" :page="page.page.value" :has-next="!!page.value.value.nextCursor" :loading="page.loading.value" @next="page.next" @previous="page.previous" />
  </section>
  <Dialog :open="!!selected" @update:open="open => { if (!open && !busy) selected = null }">
    <DialogContent class="max-h-[85vh] overflow-auto sm:max-w-2xl">
      <DialogHeader><DialogTitle>{{ selected?.metadata.displayName }} releases</DialogTitle><DialogDescription>Choose a retained release explicitly. Hive checks its required data contracts and portable service interfaces before changing the active pointer.</DialogDescription></DialogHeader>
      <div v-if="selected" class="space-y-4">
        <p class="text-sm">Current release: <strong>{{ selected.currentReleaseId ?? 'None' }}</strong></p>
        <RemoteState :loading="releases.loading.value" :error="releases.error.value" :has-data="!!releases.value.value" @retry="releases.refresh" />
        <div><Label for="release-choice">Retained release</Label><OptionSelect id="release-choice" v-model="releaseId" :disabled="!!pending || busy" class="mt-2">
          <option value="">Inspect current release</option><option v-if="releaseId && !releases.value.value?.items.some(item => item.releaseId === releaseId)" :value="releaseId">{{ releaseId }}</option>
          <option v-for="release in releases.value.value?.items ?? []" :key="release.releaseId" :value="release.releaseId">{{ release.releaseId }} · {{ release.assetCount }} assets{{ release.releaseId === selected.currentReleaseId ? ' (current)' : '' }}</option>
        </OptionSelect></div>
        <PageControls v-if="releases.value.value" :count="releases.value.value.items.length" :page="releases.page.value" :has-next="!!releases.value.value.nextCursor" :loading="releases.loading.value" @next="releases.next" @previous="releases.previous" />
        <RemoteState :loading="inspection.loading.value" :error="inspection.error.value" :has-data="!!inspected" @retry="inspection.refresh" />
        <div v-if="inspected" class="space-y-3 rounded-md border p-4">
          <div class="flex flex-wrap items-center justify-between gap-3"><StatusBadge :label="statuses[inspected.status]" :tone="inspected.status === 'ready' ? 'good' : 'warning'" /><span class="text-xs text-muted-foreground">Checked {{ dateLabel(inspected.checkedAt) }}</span></div>
          <p v-if="inspected.release" class="break-all text-sm">{{ inspected.release.releaseId }} · {{ inspected.release.entryPath }} · {{ inspected.release.assetCount }} assets</p>
          <div v-for="(issue, index) in inspected.issues" :key="index" class="rounded-md bg-muted p-3 text-sm"><p>{{ issue.message }}</p><p class="mt-1 break-all font-mono text-xs text-muted-foreground">{{ issue.code }} · {{ Object.values(issue.resource).join(' · ') }}</p></div>
          <p v-if="inspected.issuesTruncated" class="text-sm">Showing the first 32 issues. More checks failed.</p>
          <Disclosure v-if="inspected.requirements" title="Exact backend requirements"><ContentView class="mt-3" :text="JSON.stringify(inspected.requirements, null, 2)" media-type="application/json" /></Disclosure>
        </div>
        <p v-if="feedback" role="status" class="rounded-md border p-3 text-sm">{{ feedback }}</p>
        <p v-if="pending && !busy" role="status" class="text-sm">Operation {{ pending.mutationId }} has an unconfirmed outcome. Its original release {{ pending.releaseId }} and expected pointer {{ pending.expectedReleaseId }} survive this tab's reload. Retry explicitly to obtain its original result.</p>
      </div>
      <DialogFooter><Button variant="outline" :disabled="busy" @click="selected = null">Close</Button><Button :disabled="busy || (!pending && (!releaseId || releaseId === selected?.currentReleaseId || inspected?.status !== 'ready' || inspection.loading.value || !!inspection.error.value))" @click="rollback">{{ busy ? 'Selecting…' : pending ? 'Retry original selection' : 'Select this release' }}</Button></DialogFooter>
    </DialogContent>
  </Dialog>
</template>
