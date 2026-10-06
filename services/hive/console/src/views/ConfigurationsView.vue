<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref, watch } from "vue";
import { Disclosure, Item,
  Button,
  ContentView,
  Input,
  Label,
  PageControls,
  PageHeader,
  RemoteState,
  StatusBadge,
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
  Textarea,
  useRemote,
} from "@ivy/ui";
import {
  IvyError,
  newOperationId,
} from "../../../../../packages/sdk/src/client";
import { consoleClient, dateLabel, shortHash, usePage } from "../runtime";
import type { Operation } from "../runtime";

const page = usePage((signal, cursor) =>
  consoleClient.request(
    "hostConfigurations.list",
    { limit: 50, ...(cursor ? { cursor } : {}) },
    { signal },
  ), undefined, ["objects/ivy/host-configuration"]);
const observations = useRemote((signal) =>
    consoleClient.request("hosts.observations", { limit: 200 }, { signal }), 10000, ["system"]);
const selected = ref<Operation.HostConfigurationEditor | null>(null),
  hostId = ref(""),
  draft = ref(""),
  loadedRevision = ref<number | null>(null);
const baseDraft = ref(""),
  restoredSecrets = ref(false),
  conflictDraft = ref<string | null>(null);
const busy = ref(false),
  feedback = ref<string | null>(null);
const draftStorageKey = "ivy.console.host-configuration-draft.v1";
const dirty = computed(() => draft.value !== baseDraft.value);
const history = usePage(async (signal, cursor) =>
  selected.value
    ? consoleClient.request(
        "hostConfigurations.history",
        {
          hostId: selected.value.hostId,
          limit: 50,
          ...(cursor ? { cursor } : {}),
        },
        { signal },
      )
    : { items: [], nextCursor: null, historyComplete: true }, undefined, ["objects/ivy/host-configuration"]);
const appliedRevision = (item: Operation.HostConfigurationSummary) =>
  observations.value.value?.items.find(
    (value) => value.snapshot.status.hostId === item.hostId,
  )?.snapshot.status.executor?.configurationRevision ?? null;

const confirmDiscard = () =>
  !dirty.value || window.confirm("Discard the unsaved configuration draft?");
const load = async (id: string, revision?: number, force = false) => {
  if (!force && !confirmDiscard()) return;
  busy.value = true;
  feedback.value = null;
  try {
    const value = await consoleClient.request("hostConfigurations.edit", {
      hostId: id,
      ...(revision ? { revision } : {}),
    });
    if (!revision) selected.value = value;
    hostId.value = value.hostId;
    loadedRevision.value = value.revision;
    draft.value = JSON.stringify(value.configuration, null, 2);
    baseDraft.value = draft.value;
    restoredSecrets.value = false;
    if (!revision) history.reset();
  } catch (error) {
    feedback.value =
      error instanceof Error
        ? error.message
        : "Configuration could not be loaded.";
  } finally {
    busy.value = false;
  }
};
const startNew = () => {
  if (!confirmDiscard()) return;
  selected.value = null;
  loadedRevision.value = null;
  hostId.value = "";
  draft.value = "";
  baseDraft.value = "";
  feedback.value = null;
  conflictDraft.value = null;
};
const initialize = () => {
  draft.value = JSON.stringify(
    { schemaVersion: 1, hostId: hostId.value.trim() },
    null,
    2,
  );
  baseDraft.value = draft.value;
};
const save = async () => {
  if (busy.value) return;
  busy.value = true;
  feedback.value = null;
  try {
    const configuration = JSON.parse(
      draft.value,
    ) as Operation.HostConfigurationsSaveParams["configuration"];
    const id = hostId.value.trim();
    const value = await consoleClient.request("hostConfigurations.save", {
      hostId: id,
      configuration,
      ...(selected.value ? { expectedRevision: selected.value.revision } : {}),
      mutationId: await newOperationId(consoleClient),
    });
    selected.value = value;
    loadedRevision.value = value.revision;
    draft.value = JSON.stringify(value.configuration, null, 2);
    baseDraft.value = draft.value;
    conflictDraft.value = null;
    restoredSecrets.value = false;
    feedback.value = `Saved revision ${value.revision}.`;
    await Promise.all([page.refresh(), history.reset()]);
  } catch (error) {
    if (error instanceof IvyError && error.code === "revision_conflict")
      conflictDraft.value = draft.value;
    feedback.value =
      error instanceof IvyError && error.code === "revision_conflict"
        ? "The configuration changed before this save. Your draft is preserved; reload the current revision, then explicitly reapply it."
        : error instanceof Error
          ? error.message
          : "Configuration could not be saved.";
  } finally {
    busy.value = false;
  }
};
const reloadAfterConflict = async () => {
  const retained = conflictDraft.value ?? draft.value;
  await load(hostId.value, undefined, true);
  conflictDraft.value = retained;
};
const reapplyConflict = () => {
  if (!conflictDraft.value) return;
  draft.value = conflictDraft.value;
  conflictDraft.value = null;
  feedback.value =
    "The prior draft was reapplied to the current base revision. Review it before saving.";
};

const pointerParts = (path: string) =>
  path === ""
    ? []
    : path
        .slice(1)
        .split("/")
        .map((part) => part.replace(/~1/g, "/").replace(/~0/g, "~"));
const safeStoredDraft = (
  text: string,
): { draft: string; redacted: boolean } => {
  let value: unknown;
  try {
    value = JSON.parse(text) as unknown;
  } catch {
    return { draft: "", redacted: false };
  }
  if (value === null || typeof value !== "object" || Array.isArray(value))
    return { draft: "", redacted: false };
  const projected = structuredClone(value) as { instances?: unknown[] };
  let redacted = false;
  for (const raw of projected.instances ?? []) {
    if (raw === null || typeof raw !== "object" || Array.isArray(raw)) continue;
    const instance = raw as Record<string, unknown>,
      instanceId = instance["instanceId"];
    const paths = [
      "/credential",
      ...(Array.isArray(instance["secretPaths"])
        ? instance["secretPaths"].filter(
            (path): path is string => typeof path === "string",
          )
        : []),
    ];
    for (const path of paths) {
      const parts = pointerParts(path);
      let parent: Record<string, unknown> | null = instance;
      for (const part of parts.slice(0, -1)) {
        const next: unknown = parent?.[part];
        parent =
          next !== null && typeof next === "object" && !Array.isArray(next)
            ? (next as Record<string, unknown>)
            : null;
      }
      if (!parent || !parts.length || !Object.hasOwn(parent, parts.at(-1)!))
        continue;
      const current = parent[parts.at(-1)!],
        container =
          current !== null &&
          typeof current === "object" &&
          !Array.isArray(current)
            ? (current as Record<string, unknown>)
            : null,
        marker = container?.["$ivySecret"];
      const safe =
        container !== null &&
        Object.keys(container).length === 1 &&
        marker !== null &&
        typeof marker === "object" &&
        !Array.isArray(marker) &&
        Object.keys(marker).length === 2 &&
        (marker as Record<string, unknown>)["instanceId"] === instanceId &&
        (marker as Record<string, unknown>)["path"] === path;
      if (!safe) {
        delete parent[parts.at(-1)!];
        redacted = true;
      }
    }
  }
  return { draft: JSON.stringify(projected, null, 2), redacted };
};
const persistDraft = () => {
  if (!draft.value && !hostId.value) {
    sessionStorage.removeItem(draftStorageKey);
    return;
  }
  const safe = safeStoredDraft(draft.value);
  const safeConflict = conflictDraft.value
    ? safeStoredDraft(conflictDraft.value)
    : null;
  sessionStorage.setItem(
    draftStorageKey,
    JSON.stringify({
      hostId: hostId.value,
      loadedRevision: loadedRevision.value,
      selected: selected.value,
      baseDraft: safeStoredDraft(baseDraft.value).draft,
      draft: safe.draft,
      conflictDraft: safeConflict?.draft || null,
      redacted: safe.redacted || safeConflict?.redacted === true,
    }),
  );
};
let acceptedHash = location.hash,
  allowNextHash = false,
  restoringHash = false;
const beforeUnload = (event: BeforeUnloadEvent) => {
  if (dirty.value) {
    event.preventDefault();
    event.returnValue = "";
  }
};
const navigationClick = (event: MouseEvent) => {
  const anchor =
    event.target instanceof Element
      ? event.target.closest<HTMLAnchorElement>("a[href]")
      : null;
  if (
    !anchor ||
    event.defaultPrevented ||
    event.button !== 0 ||
    event.metaKey ||
    event.ctrlKey ||
    event.shiftKey ||
    event.altKey
  )
    return;
  const target = new URL(anchor.href, location.href);
  if (
    target.origin !== location.origin ||
    target.pathname !== location.pathname ||
    target.search !== location.search ||
    !target.hash ||
    target.hash === location.hash
  )
    return;
  if (!confirmDiscard()) {
    event.preventDefault();
    event.stopImmediatePropagation();
    return;
  }
  allowNextHash = true;
};
const hashNavigation = () => {
  if (restoringHash) {
    restoringHash = false;
    return;
  }
  if (allowNextHash) {
    allowNextHash = false;
    acceptedHash = location.hash;
    return;
  }
  if (
    !dirty.value ||
    window.confirm("Discard the unsaved configuration draft?")
  ) {
    acceptedHash = location.hash;
    return;
  }
  restoringHash = true;
  location.hash = acceptedHash;
};
onMounted(() => {
  window.addEventListener("beforeunload", beforeUnload);
  window.addEventListener("hashchange", hashNavigation);
  document.addEventListener("click", navigationClick, true);
  try {
    const saved = JSON.parse(
      sessionStorage.getItem(draftStorageKey) ?? "null",
    ) as {
      hostId?: string;
      loadedRevision?: number | null;
      selected?: Operation.HostConfigurationEditor | null;
      baseDraft?: string;
      draft?: string;
      conflictDraft?: string | null;
      redacted?: boolean;
    } | null;
    if (saved?.draft) {
      hostId.value = saved.hostId ?? "";
      loadedRevision.value = saved.loadedRevision ?? null;
      selected.value = saved.selected ?? null;
      baseDraft.value = saved.baseDraft ?? "";
      draft.value = saved.draft;
      conflictDraft.value = saved.conflictDraft ?? null;
      restoredSecrets.value = saved.redacted === true;
      feedback.value = saved.redacted
        ? "Draft restored after refresh. Newly entered secrets were not stored and must be entered again."
        : "Draft restored after refresh.";
    }
  } catch {
    sessionStorage.removeItem(draftStorageKey);
  }
});
onBeforeUnmount(() => {
  window.removeEventListener("beforeunload", beforeUnload);
  window.removeEventListener("hashchange", hashNavigation);
  document.removeEventListener("click", navigationClick, true);
});
watch([hostId, draft, conflictDraft, loadedRevision, selected], persistDraft, {
  deep: true,
});
</script>
<template>
  <PageHeader
    title="Settings"
    description="View and change each host's complete desired configuration. HostExecutors converge to the current revision while keeping their last valid local state offline."
    :loading="page.loading.value"
    :updated-at="page.updatedAt.value"
  />
  <RemoteState
    :loading="page.loading.value"
    :error="page.error.value"
    :has-data="!!page.value.value"
    :empty="page.value.value?.items.length === 0"
    empty-title="No central host configurations"
    empty-detail="Create the first desired host configuration from its complete JSON document."
    @retry="page.refresh"
  />
  <div
    v-if="page.value.value?.items.length"
    class="mb-6 overflow-hidden rounded-xl border bg-card"
  >
    <Table
      ><TableHeader
        ><TableRow
          ><TableHead>Host</TableHead><TableHead>Revision</TableHead
          ><TableHead>Updated</TableHead
          ><TableHead><span class="sr-only">Action</span></TableHead></TableRow
        ></TableHeader
      ><TableBody>
        <TableRow v-for="item in page.value.value.items" :key="item.hostId"
          ><TableCell class="font-medium">{{ item.hostId }}</TableCell
          ><TableCell
            ><StatusBadge
              :label="'Desired revision ' + item.revision"
            /><StatusBadge
              class="ml-2"
              :label="
                appliedRevision(item) === item.revision
                  ? 'Applied'
                  : appliedRevision(item)
                    ? 'Applied ' + appliedRevision(item)
                    : 'Not observed'
              "
              :tone="
                appliedRevision(item) === item.revision ? 'good' : 'warning'
              "
            />
            <p class="mt-1 font-mono text-xs text-muted-foreground">
              {{ shortHash(item.contentHash) }}
            </p></TableCell
          ><TableCell class="text-sm">{{ dateLabel(item.updatedAt) }}</TableCell
          ><TableCell
            ><Button
              size="sm"
              variant="outline"
              :disabled="busy"
              @click="load(item.hostId)"
              >Edit</Button
            ></TableCell
          ></TableRow
        >
      </TableBody></Table
    >
  </div>
  <PageControls
    v-if="page.value.value"
    :count="page.value.value.items.length"
    :page="page.page.value"
    :has-next="!!page.value.value.nextCursor"
    :loading="page.loading.value"
    @next="page.next"
    @previous="page.previous"
  />

  <section class="mt-8 space-y-4 rounded-xl border bg-card p-5">
    <div class="flex flex-wrap items-end gap-3">
      <div class="min-w-64 flex-1">
        <Label for="configuration-host">Host ID</Label
        ><Input
          id="configuration-host"
          v-model="hostId"
          class="mt-2"
          :disabled="!!selected || busy"
          placeholder="host-01"
        />
      </div>
      <Button variant="outline" :disabled="busy" @click="startNew"
        >New host</Button
      ><Button
        v-if="!selected"
        variant="outline"
        :disabled="busy || !hostId.trim()"
        @click="initialize"
        >Initialize JSON</Button
      ><Button
        v-if="selected"
        variant="outline"
        :disabled="busy"
        @click="load(selected.hostId)"
        >Reload current</Button
      >
    </div>
    <div>
      <div class="mb-2 flex flex-wrap items-center justify-between gap-2">
        <Label for="configuration-json">Complete HostConfig JSON</Label
        ><span class="text-xs text-muted-foreground"
          ><template v-if="loadedRevision"
            >Loaded revision {{ loadedRevision
            }}{{
              selected && loadedRevision !== selected.revision
                ? " · saving creates a new revision from current " +
                  selected.revision
                : ""
            }}</template
          >{{ dirty ? " · unsaved draft" : "" }}</span
        >
      </div>
      <p class="mb-3 text-sm text-muted-foreground">
        Each instance credential and every setting listed in that instance's
        <code>secretPaths</code> stays server-side. An
        <code>$ivySecret</code> marker names its original instance and JSON
        Pointer; leave it unchanged to preserve that exact value.
      </p>
      <p v-if="restoredSecrets" class="mb-3 text-sm text-warning">
        Newly entered plaintext secrets are never kept in browser draft storage;
        re-enter the omitted values before saving.
      </p>
      <Textarea
        id="configuration-json"
        v-model="draft"
        class="min-h-[32rem] font-mono text-xs"
        spellcheck="false"
        :disabled="busy"
      />
    </div>
    <p v-if="feedback" role="status" class="rounded-md border p-3 text-sm">
      {{ feedback }}
    </p>
    <div class="flex flex-wrap justify-end gap-2">
      <Button
        v-if="conflictDraft"
        variant="outline"
        :disabled="busy"
        @click="reloadAfterConflict"
        >Reload current and keep draft</Button
      ><Button
        v-if="conflictDraft && loadedRevision === selected?.revision"
        variant="outline"
        :disabled="busy"
        @click="reapplyConflict"
        >Reapply prior draft</Button
      ><Button
        :disabled="busy || !hostId.trim() || !draft.trim()"
        @click="save"
        >{{
          busy
            ? "Saving…"
            : selected
              ? "Save new revision"
              : "Create configuration"
        }}</Button
      >
    </div>
  </section>

  <section v-if="selected" class="mt-8">
    <h2 class="text-lg font-semibold">Revision history</h2>
    <RemoteState
      class="mt-3"
      :loading="history.loading.value"
      :error="history.error.value"
      :has-data="!!history.value.value"
      @retry="history.refresh"
    />
    <div class="mt-3 grid gap-2">
      <Item
        v-for="revision in history.value.value?.items ?? []"
        :key="revision.revision"
        as-child
        variant="outline"
        size="sm"
        ><button
          type="button"
          class="text-left hover:bg-accent disabled:opacity-50"
          :disabled="busy"
          @click="load(selected.hostId, revision.revision)"
        >
        <strong>Revision {{ revision.revision }}</strong
        ><span class="ml-3 text-muted-foreground"
          >{{ dateLabel(revision.createdAt) }} ·
          {{ revision.byteLength.toLocaleString() }} bytes ·
          {{ shortHash(revision.contentHash) }}</span
        >
      </button></Item
      >
    </div>
    <PageControls
      v-if="history.value.value"
      :count="history.value.value.items.length"
      :page="history.page.value"
      :has-next="!!history.value.value.nextCursor"
      :loading="history.loading.value"
      @next="history.next"
      @previous="history.previous"
    />
    <Disclosure class="mt-4" title="Current retained record">
      <ContentView
        class="mt-3"
        :text="JSON.stringify(selected, null, 2)"
        media-type="application/json"
      />
    </Disclosure>
  </section>
</template>
