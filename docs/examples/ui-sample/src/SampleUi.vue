<script setup lang="ts">
import { computed, onBeforeUnmount, ref } from 'vue';
import { browserClient } from '@ivy/sdk';
import { IvyShell, PageHeader, RemoteState, Label, Textarea, ContentView, useRemote } from '@ivy/ui';

const suffix = '/ui/ui-sample/', offset = location.pathname.lastIndexOf(suffix);
if (offset < 0 || !/^(?:index\.html)?$/.test(location.pathname.slice(offset + suffix.length))) throw new Error('Open the installed UI from Hive Console.');
const base = new URL(location.pathname.slice(0, offset) + '/', location.origin), client = browserClient(base.href);
const logoutUrl = new URL('logout', base).href;
const hash = ref(location.hash || '#/preview');
const routeChanged = () => { hash.value = location.hash || '#/preview'; };
window.addEventListener('hashchange', routeChanged); onBeforeUnmount(() => window.removeEventListener('hashchange', routeChanged));
const page = computed(() => hash.value === '#/about' ? 'about' : 'preview');
const status = useRemote(signal => client.request('system.status', {}, { signal }), 15000);
const connection = computed(() => status.error.value ? 'Hive unavailable' : status.value.value?.ready ? 'Hive ready' : 'Connecting');
const draft = ref('# A working note\n\nThe shared components keep this preview consistent with the workspace.');
const refresh = () => { void status.refresh(); };
</script>
<template>
  <IvyShell ui-name="UI sample" :home-url="base.href" :logout-url="logoutUrl"
    :connection="connection"
    :connection-tone="status.error.value ? 'bad' : status.value.value?.ready ? 'good' : 'neutral'"
    :navigation="[{label:'Preview',href:'#/preview',active:page==='preview'},{label:'About',href:'#/about',active:page==='about'}]">
    <PageHeader title="UI sample" :description="page === 'preview' ? 'Try a local note with the workspace’s shared controls and safe preview.' : 'A small UI that demonstrates the shared package boundary.'"
      :loading="status.loading.value" :updated-at="status.updatedAt.value" @refresh="refresh" />
    <RemoteState :loading="status.loading.value" :error="status.error.value" :has-data="!!status.value.value" @retry="status.refresh" />
    <template v-if="page === 'preview'">
      <div class="grid gap-6 xl:grid-cols-2">
        <section class="min-w-0 rounded-xl border bg-card p-5">
          <Label for="draft">Working note</Label>
          <Textarea id="draft" v-model="draft" class="mt-3 min-h-64" aria-describedby="draft-scope" />
          <p id="draft-scope" class="mt-3 text-sm text-muted-foreground">This preview stays in this open page. It does not save or send your note.</p>
        </section>
        <section class="min-w-0 rounded-xl border bg-card p-5" aria-label="Note preview">
          <h2 class="mb-4 font-semibold">Preview</h2><ContentView :text="draft" media-type="text/markdown" :maximum-characters="65536" />
        </section>
      </div>
    </template>
    <section v-else class="max-w-3xl space-y-4 rounded-xl border bg-card p-6">
      <h2 class="text-lg font-semibold">One shared toolkit</h2>
      <p>This UI uses the same shell, controls, theme, safe content viewer and connection states as the built-in workspace UIs.</p>
      <p>It uses the public SDK for read-only Hive status.</p>
      <p v-if="status.value.value" class="text-sm text-muted-foreground">Hive {{ status.value.value.version }} · Observed {{ status.updatedAt.value }}</p>
    </section>
  </IvyShell>
</template>
