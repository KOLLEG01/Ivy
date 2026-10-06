<script setup lang="ts">
import { Card, CardDescription, CardHeader, CardTitle, PageControls, PageHeader, RemoteState, StatusBadge, uiIcon } from '@ivy/ui';
import { uiUrl, consoleClient, usePage } from '../runtime';

const page = usePage((signal, cursor) => consoleClient.request('uis.catalog', { limit: 50, ...(cursor ? { cursor } : {}) }, { signal }), undefined, ["uis"]);
</script>
<template>
  <PageHeader title="UIs" description="Open an Ivy web UI." :loading="page.loading.value" :updated-at="page.updatedAt.value" />
  <RemoteState :loading="page.loading.value" :error="page.error.value" :has-data="!!page.value.value" :empty="page.value.value?.items.filter(ui => ui.metadata.uiId !== 'chat-ui').length === 0" empty-title="Your workspace starts here" empty-detail="No web UIs are installed yet. System remains available from the navigation." @retry="page.refresh" />
  <div class="grid gap-5 md:grid-cols-2 xl:grid-cols-3">
    <component :is="ui.currentReleaseId ? 'a' : 'div'" v-for="ui in page.value.value?.items.filter(value => value.metadata.uiId !== 'chat-ui') ?? []" :key="ui.metadata.uiId"
      :href="ui.currentReleaseId ? uiUrl(ui) : undefined" :role="ui.currentReleaseId ? undefined : 'link'" :aria-disabled="ui.currentReleaseId ? undefined : 'true'" :aria-label="'Open ' + ui.metadata.displayName"
      class="group min-w-0 rounded-xl outline-none focus-visible:ring-3 focus-visible:ring-ring/50" :class="ui.currentReleaseId ? 'cursor-pointer' : 'cursor-not-allowed'">
      <Card class="h-full gap-3 transition-colors" :class="ui.currentReleaseId ? 'group-hover:bg-accent/50 group-hover:border-foreground/20' : 'opacity-70'">
        <CardHeader>
          <div class="flex items-center justify-between gap-3"><span class="flex size-9 items-center justify-center rounded-md bg-secondary text-primary"><component :is="uiIcon(ui.metadata.iconKey)" class="size-4" aria-hidden="true" /></span><StatusBadge v-if="!ui.currentReleaseId" label="No active release" tone="warning" /></div>
          <CardTitle class="mt-2">{{ ui.metadata.displayName }}</CardTitle>
          <CardDescription>{{ ui.metadata.description }}</CardDescription>
        </CardHeader>
      </Card>
    </component>
  </div>
  <PageControls v-if="page.value.value && (page.value.value.nextCursor || page.page.value > 1)" :count="page.value.value.items.length" :page="page.page.value" :has-next="!!page.value.value.nextCursor" :loading="page.loading.value" @next="page.next" @previous="page.previous" />
</template>
