<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, ref, watch } from 'vue';
import { Clock, Copy, Download, FilePlus2, FolderInput, History, Link2, MoreHorizontal, Paperclip } from '@lucide/vue';
import { Alert, AlertDescription, BreadcrumbTrail, Button, ContentView, DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuSeparator, DropdownMenuTrigger, ImagePreview, Input, Label, MarkdownEditor, PageControls, RemoteState, RevisionDiff, Sheet, SheetContent, SheetDescription, SheetHeader, SheetTitle, Tabs, TabsContent, TabsList, TabsTrigger, ToolbarContent, attachmentMarkdown, fileSize, useRemote } from '@ivy/ui';
import type { AttachmentInfo, UploadedAttachment } from '@ivy/ui';
import { dateLabel, IvyError, route, usePage } from '../../../packages/ui-client/src/runtime';
import type { Operation } from '../../../packages/ui-client/src/runtime';
import { downloadRevision } from '../../../packages/ui-client/src/content';
import { uiUrl, base, client } from './runtime';
import { useObjectArchive } from '../../../packages/ui-client/src/object-archive';
import WikiAttachments from './WikiAttachments.vue';
import PageLocation from './PageLocation.vue';
import ObjectDeleteDialog from '../../../packages/ui-client/src/ObjectDeleteDialog.vue';
import { newOperationId } from '../../../packages/sdk/src/client';
import { imageMime, verifiedImageUrl } from './wiki-media';
import { useWikiUpload } from './wiki-upload';

const pageLocation = ref<InstanceType<typeof PageLocation>>(), deleteOpen = ref(false);
const detailsOpen = ref(false), detailsTab = ref('attachments');
const props = defineProps<{ query: URLSearchParams; creating: boolean; ancestry: Array<{ id: string; label: string; href: string }>; ancestryError: string | null }>();
const id = props.query.get('id'), parentId = props.query.get('parent'), revisionText = props.query.get('revision');
const uploads = useWikiUpload(id), editor = ref<InstanceType<typeof MarkdownEditor>>();
const attachmentsAttention = computed(() => !!uploads.pending.value || !!uploads.error.value);
const historical = props.query.has('revision'), revision = revisionText && /^[1-9]\d*$/.test(revisionText) && Number.isSafeInteger(Number(revisionText)) ? Number(revisionText) : undefined;
const readableText = (value: Operation.ObjectRead | null) => !value ? '' : value.content.encoding === 'text' ? value.content.value : value.content.encoding === 'json' ? JSON.stringify(value.content.value, null, 2) : 'This is a binary attachment. Download its exact saved revision to inspect it.';
const detail = useRemote(signal => historical && revision === undefined ? Promise.reject(new Error('This link has an invalid revision.')) : id && !props.creating ? client.request('objects.read', { objectId: id, ...(revision ? { revision } : {}) }, { signal }) : Promise.resolve(null), 0, historical || props.creating ? undefined : ["objects"]);
const trustedImageUrls = ref<Record<string, string>>({}), trustedAttachments = ref<Record<string, AttachmentInfo>>({});
const attachmentPreview = ref<string | null>(null), previewOpen = ref(false), imageError = ref<string | null>(null), mediaLoading = ref(false);
const mediaCache = new Map<string, Operation.ObjectRead>();
const history = usePage(async (signal, cursor) => id ? client.request('objects.history', { objectId: id, limit: 50, ...(cursor ? { cursor } : {}) }, { signal }) : { items: [], nextCursor: null }, 0, undefined, ["objects/wiki/page", "objects/wiki/attachment"]);
const refresh = () => { void detail.refresh(); void history.refresh(); };
const writable = computed(() => !historical && detail.value.value?.object.contractKey === 'wiki/page' && detail.value.value.revision.contractVersion === '1.0.0' && detail.value.value.content.encoding === 'text' && !detail.value.value.object.effectivelyArchived);
interface Draft { title: string; baseTitle: string; body: string; baseBody?: string; baseRevision: number | null; operationId: string | null; attachments: Operation.ObjectRead['revision']['references']; saving?: { title: string; body: string; references: Operation.ObjectRead['revision']['references'] } }
const draft = ref<Draft | null>(null), busy = ref(false), message = ref<string | null>(null), storageError = ref<string | null>(null);
const conflict = ref<Operation.ObjectRead | null>(null), compareRevision = ref(''), comparison = ref<Operation.ObjectRead | null>(null);
// Autosave state: `offline` keeps retrying the retained save; `blocked` waits for the user to change what was refused.
const saveState = ref<'saved' | 'pending' | 'saving' | 'offline' | 'blocked'>('saved'), savedRevision = ref<number | null>(null);
const key = 'ivy.wiki.draft:' + base.href + ':' + (props.creating ? 'new:' + (parentId ?? 'root') : id);
const baseline = (value: Operation.ObjectRead): Draft => ({ title: value.object.name, baseTitle: value.object.name, body: readableText(value), baseBody: readableText(value), baseRevision: value.revision.revision, operationId: null, attachments: {} });
const editorLocked = computed(() => !props.creating && !writable.value);
// A created page hands over to its page view; this "new page" draft is finished.
const created = ref(false);
let active = true;
// The editor may serialize saved Markdown differently without any user edit; that form is not a change.
const editorBaseline = ref<{ source: string; normalized: string } | null>(null);
const editorNormalized = (normalized: string, source: string) => { editorBaseline.value = { source, normalized }; };
const savedBody = (value: Draft) => value.baseBody ?? (detail.value.value?.revision.revision === value.baseRevision ? readableText(detail.value.value) : null);
const unchangedBody = (body: string, saved: string | null) => saved !== null && (body === saved || (editorBaseline.value?.source === saved && body === editorBaseline.value.normalized));

const dirty = computed(() => {
  if (!draft.value) return false;
  if (props.creating) return !created.value && !!(draft.value.title || draft.value.body || draft.value.operationId);
  return draft.value.title !== draft.value.baseTitle || !unchangedBody(draft.value.body, savedBody(draft.value)) || Object.keys(draft.value.attachments).length > 0 || !!draft.value.operationId;
});
// Drafts live in localStorage so a closed tab or crash cannot lose typed text before it is saved.
const storage = { get: () => localStorage.getItem(key) ?? sessionStorage.getItem(key), set: (value: string) => { localStorage.setItem(key, value); sessionStorage.removeItem(key); },
  remove: () => { localStorage.removeItem(key); sessionStorage.removeItem(key); } };
if (!historical) {
  try {
    const raw = storage.get(), saved = raw && raw.length < 3 * 1024 * 1024 ? JSON.parse(raw) as Partial<Draft> & { handoff?: boolean } : null;
    if (saved && typeof saved.title === 'string' && typeof saved.body === 'string' && (saved.baseRevision === null || Number.isSafeInteger(saved.baseRevision)) && (saved.operationId === null || saved.operationId === undefined || typeof saved.operationId === 'string')) {
      draft.value = { title: saved.title, baseTitle: saved.baseTitle ?? saved.title, body: saved.body, ...(typeof saved.baseBody === 'string' ? { baseBody: saved.baseBody } : {}), baseRevision: saved.baseRevision ?? null, operationId: saved.operationId ?? null, attachments: saved.attachments ?? {}, ...(saved.saving ? { saving: saved.saving } : {}) };
      // A page created by autosave hands its latest keystrokes to the page view without a restore notice.
      if (!saved.handoff) message.value = 'Your unsaved changes were restored and are being saved.';
    }
  } catch { storageError.value = 'This browser could not restore the saved local draft.'; }
  if (props.creating && !draft.value) draft.value = { title: '', baseTitle: '', body: '', baseBody: '', baseRevision: null, operationId: null, attachments: {} };
}
const imageUrls = new Map<string, string>();
onBeforeUnmount(() => { for (const url of imageUrls.values()) URL.revokeObjectURL(url); });
watch(() => [detail.value.value, draft.value?.attachments] as const, async ([value, added], _previous, onCleanup) => {
  let cancelled = false;
  const controller = new AbortController();
  onCleanup(() => { cancelled = true; controller.abort(); });
  imageError.value = null;
  if (!value) return;
  if (value.object.contractKey === 'wiki/attachment') {
    const source = value.object.id + ':' + value.revision.revision;
    if (imageUrls.has(source)) { attachmentPreview.value = imageUrls.get(source)!; return; }
    attachmentPreview.value = null; mediaLoading.value = true;
    try {
      const url = await verifiedImageUrl(value);
      if (url) { imageUrls.set(source, url); if (!cancelled) attachmentPreview.value = url; }
    } catch (cause) { if (!cancelled) imageError.value = cause instanceof Error ? cause.message : 'Preview unavailable.'; }
    finally { if (!cancelled) mediaLoading.value = false; }
    return;
  }
  // Verified images stay rendered across live refreshes; only references not seen before are loaded.
  const references = Object.entries({ ...value.revision.references, ...added }).filter(([name]) => name.startsWith('attachment.'))
    .map(([, reference]) => ({ reference, source: route('page', { id: reference.objectId, revision: String(reference.revision) }) }))
    .filter(({ source }) => !trustedAttachments.value[source]);
  let offset = 0, failures = 0;
  const loadNext = async () => {
    while (!cancelled && offset < references.length) {
      const { reference, source } = references[offset++]!;
      try {
        const attachment = mediaCache.get(source) ?? await client.request('objects.read', { objectId: reference.objectId, revision: reference.revision }, { signal: controller.signal });
        if (attachment.object.contractKey !== 'wiki/attachment' || attachment.object.parentId !== value.object.id || attachment.object.ownerObjectId !== value.object.id) throw new Error('Attachment owner mismatch.');
        const url = imageUrls.get(source) ?? await verifiedImageUrl(attachment);
        if (url) imageUrls.set(source, url);
        if (cancelled) return;
        mediaCache.set(source, attachment);
        trustedAttachments.value = { ...trustedAttachments.value, [source]: { name: attachment.object.name, byteLength: attachment.revision.byteLength } };
        if (url) trustedImageUrls.value = { ...trustedImageUrls.value, [source]: url };
      } catch { if (!cancelled) failures++; }
    }
  };
  if (!references.length) return;
  mediaLoading.value = true;
  await Promise.all(Array.from({ length: Math.min(4, references.length) }, loadNext));
  if (!cancelled) { mediaLoading.value = false; if (failures) imageError.value = `${failures} attachment(s) could not be loaded. Use Retry attachments; other attachments remain available.`; }
}, { immediate: true });

const persist = () => {
  if (!draft.value) return;
  try {
    if (dirty.value) storage.set(JSON.stringify(draft.value)); else storage.remove();
    storageError.value = null;
  } catch { storageError.value = 'Local draft storage is unavailable. Keep this page open until your changes are saved.'; }
};

// Save shortly after typing pauses, and at least every 15 seconds while typing continues.
const idleDelay = 1200, longestDelay = 15000;
let timer: ReturnType<typeof setTimeout> | undefined, firstChange = 0, retryDelay = 0, again = false, refusedTitle: string | null = null;
const canSave = () => !!draft.value && dirty.value && !conflict.value && !!draft.value.title.trim() && draft.value.title !== refusedTitle &&
  (props.creating || (!!id && draft.value.baseRevision !== null && (writable.value || !!draft.value.operationId)));
const schedule = (delay?: number) => {
  clearTimeout(timer);
  if (!canSave()) { if (!busy.value && saveState.value !== 'blocked') saveState.value = dirty.value ? 'pending' : 'saved'; return; }
  if (saveState.value !== 'offline' && saveState.value !== 'saving') saveState.value = 'pending';
  const now = Date.now(); firstChange ||= now;
  timer = setTimeout(() => void save(), delay ?? Math.min(idleDelay, Math.max(0, firstChange + longestDelay - now)));
};
watch(draft, () => { persist(); if (busy.value) again = true; else schedule(retryDelay || undefined); }, { deep: true, flush: 'sync' });
watch(() => detail.value.value, (value) => {
  if (!value || historical || !writable.value) return;
  if (!draft.value) { draft.value = baseline(value); return; }
  if (draft.value.baseBody === undefined && draft.value.baseRevision === value.revision.revision) draft.value.baseBody = readableText(value);
  // A clean draft follows newer saved revisions; a draft with local changes keeps them and meets any conflict on save.
  if (!dirty.value && !busy.value && (draft.value.baseRevision ?? 0) <= value.revision.revision) {
    const next = baseline(value);
    if (unchangedBody(draft.value.body, next.body)) next.body = draft.value.body;
    draft.value = next;
  }
  // Restored or retained changes resume saving as soon as the page they build on is known.
  else if (dirty.value && !busy.value && saveState.value !== 'blocked') schedule(retryDelay || undefined);
}, { immediate: true });

const save = async () => {
  clearTimeout(timer);
  if (busy.value) { again = true; return; }
  if (!canSave()) { schedule(); return; }
  const value = draft.value!, current = detail.value.value;
  busy.value = true; again = false; firstChange = 0; saveState.value = 'saving';
  try {
    if (props.creating && parentId && !value.operationId) {
      const parent = await client.request('objects.read', { objectId: parentId });
      if (parent.object.contractKey !== 'wiki/page' || parent.revision.contractVersion !== '1.0.0' || parent.object.effectivelyArchived) throw new Error('Choose a current writable Wiki page as the parent.');
    }
    // A retried save sends exactly what the first attempt sent; later edits wait for the next save.
    if (!value.operationId || !value.saving) value.saving = { title: props.creating || value.title !== value.baseTitle ? value.title.trim() : value.title, body: value.body,
      references: { ...(props.creating ? {} : current?.revision.references ?? {}), ...value.attachments } };
    value.operationId ??= await newOperationId(client); persist();
    const saving = value.saving;
    const target = props.creating ? { create: { contractKey: 'wiki/page', name: saving.title, parentId: parentId ?? null, ownerObjectId: null } }
      : { objectId: id!, expectedRevision: value.baseRevision!, expectedArchived: false, ...(saving.title !== value.baseTitle ? { name: saving.title } : {}) };
    const result = await client.request('objects.write', { ...target, mutationId: value.operationId, contractVersion: '1.0.0', references: saving.references, content: { encoding: 'text', value: saving.body } });
    const saved = new Set(Object.keys(saving.references));
    value.attachments = Object.fromEntries(Object.entries(value.attachments).filter(([name]) => !saved.has(name)));
    value.baseRevision = result.revision.revision; value.baseTitle = saving.title; value.baseBody = saving.body; value.operationId = null; delete value.saving;
    if (value.title.trim() === saving.title) value.title = saving.title;
    retryDelay = 0; refusedTitle = null; conflict.value = null; savedRevision.value = result.revision.revision;
    if (message.value && !storageError.value) message.value = null;
    window.dispatchEvent(new Event('ivy:objects-changed'));
    if (props.creating) {
      // The page view takes over; edits typed during the create continue there from local storage.
      const handoff = 'ivy.wiki.draft:' + base.href + ':' + result.object.id;
      created.value = true; storage.remove();
      if (value.title !== saving.title || value.body !== saving.body || Object.keys(value.attachments).length)
        try { localStorage.setItem(handoff, JSON.stringify({ ...value, handoff: true })); } catch { /* The saved revision remains; only unsent keystrokes are at risk. */ }
      if (active) location.hash = route('page', { id: result.object.id });
      return;
    }
    persist();
    if (active) { void detail.refresh(); void history.refresh(); }
  } catch (cause) {
    const failure = IvyError.from(cause);
    message.value = cause instanceof Error ? cause.message : 'The save could not be confirmed.';
    if (cause instanceof IvyError && cause.outcome === 'not_executed') {
      value.operationId = null; delete value.saving; retryDelay = 0;
      if (cause.code === 'revision_conflict' && id) {
        try {
          const latest = await client.request('objects.read', { objectId: id });
          detail.value.value = latest;
          conflict.value = latest.revision.revision === value.baseRevision ? null : latest;
        }
        catch { message.value += ' Current content is temporarily unavailable; your changes are kept on this device.'; }
      } else refusedTitle = value.title;
      saveState.value = 'blocked';
    } else {
      // The outcome is unknown: keep the exact request and retry it with growing pauses.
      retryDelay = Math.min(30000, Math.max(2000, retryDelay * 2));
      saveState.value = 'offline';
      message.value = (failure.message || 'The save could not be confirmed.') + ' Your changes are kept on this device and saving is retried automatically.';
    }
  } finally {
    busy.value = false; persist();
    if (saveState.value === 'saving') saveState.value = dirty.value ? 'pending' : 'saved';
    if (active && (again || retryDelay || dirty.value)) schedule(retryDelay || undefined);
  }
};
// Unsaved changes are already in local storage and resume after a reload; leaving only starts the save early.
const flush = () => { if (dirty.value && !busy.value) void save(); };
window.addEventListener('beforeunload', flush);
onBeforeUnmount(() => { active = false; window.removeEventListener('beforeunload', flush); if (dirty.value && !busy.value) void save(); });
const manualSave = () => { refusedTitle = null; void save(); };
const statusLabel = computed(() => saveState.value === 'saving' ? 'Saving…' : saveState.value === 'offline' ? 'Saving paused, retrying'
  : saveState.value === 'blocked' || (dirty.value && !props.creating && !writable.value && !draft.value?.operationId) ? 'Not saved'
  : dirty.value ? (draft.value?.title.trim() ? 'Unsaved changes' : 'Add a title to save') : savedRevision.value ? 'Saved revision ' + savedRevision.value + '.' : props.creating ? '' : 'Saved');
const rebase = () => {
  if (!draft.value || !conflict.value || conflict.value.object.contractKey !== 'wiki/page' || conflict.value.revision.contractVersion !== '1.0.0') return;
  const current = conflict.value;
  if (draft.value.title === draft.value.baseTitle) draft.value.title = current.object.name;
  draft.value.baseTitle = current.object.name; draft.value.baseRevision = current.revision.revision; draft.value.baseBody = readableText(current);
  draft.value.operationId = null; delete draft.value.saving; detail.value.value = current; conflict.value = null; saveState.value = 'pending';
  message.value = 'Your changes now build on revision ' + current.revision.revision + ' and are saved automatically.';
  schedule();
};
const discard = () => {
  if (!draft.value || busy.value) return;
  storage.remove(); message.value = null; saveState.value = 'saved';
  if (props.creating) { location.hash = route('home', { parent: parentId ?? '' }); return; }
  const current = conflict.value ?? detail.value.value; conflict.value = null;
  if (current) { detail.value.value = current; draft.value = baseline(current); }
};
const rememberAttachment = (value: Operation.ObjectRead): UploadedAttachment => {
  if (!draft.value || value.object.ownerObjectId !== id || value.object.parentId !== id) throw new Error('Choose an attachment owned by this page.');
  const src = route('page', { id: value.object.id, revision: String(value.revision.revision) });
  mediaCache.set(src, value);
  draft.value.attachments = { ...draft.value.attachments, ['attachment.' + value.object.id]: { objectId: value.object.id, revision: value.revision.revision } };
  return { src, name: value.object.name, image: value.content.encoding === 'base64' && !!imageMime(Uint8Array.from(atob(value.content.value.slice(0, 32)), character => character.charCodeAt(0))) };
};
const uploadFile = async (file: File) => {
  if (props.creating) throw new Error('Give the page a title first; images and files can be added once it is saved.');
  if (!writable.value) throw new Error('This page cannot receive attachments right now.');
  const value = await uploads.upload(file);
  return rememberAttachment(value);
};
const inserted = () => { schedule(); };
const insertAttachment = async (value: UploadedAttachment | UploadedAttachment[]) => {
  const focus = !uploads.error.value;
  if (focus) detailsOpen.value = false;
  await nextTick();
  editor.value?.insertMarkdown((Array.isArray(value) ? value : [value]).map(attachmentMarkdown).join('\n\n'), focus);
  inserted();
};
const insertExisting = async (value: Operation.ObjectRead) => { await insertAttachment(rememberAttachment(value)); };
const bodyInput = (body: string) => { if (draft.value && !editorLocked.value) draft.value.body = body; };
// The editable title owns its text while the user types; rendering it reactively would replace the
// text node on every keystroke and move the caret to the start. Outside changes are copied in here.
const titleElement = ref<HTMLElement>();
const caretToEnd = (element: HTMLElement) => {
  if (document.activeElement !== element) return;
  const range = document.createRange(); range.selectNodeContents(element); range.collapse(false);
  const selection = getSelection(); selection?.removeAllRanges(); selection?.addRange(range);
};
watch([titleElement, () => draft.value?.title], ([element, title]) => { if (element && element.textContent !== (title ?? '')) element.textContent = title ?? ''; }, { flush: 'post' });
const titleInput = (event: Event) => {
  if (!draft.value || editorLocked.value) return; const element = event.currentTarget as HTMLElement, next = (element.textContent ?? '').replace(/[\r\n]+/g, ' ').slice(0, 255).toWellFormed();
  if (element.textContent !== next) { element.textContent = next; caretToEnd(element); } draft.value.title = next;
};
const openDetails = (tab: string) => { detailsTab.value = tab; detailsOpen.value = true; };
const copyLink = () => void navigator.clipboard?.writeText(pageLink.value);
const focusDocument = () => document.querySelector<HTMLElement>('.wiki-document .ProseMirror')?.focus();
const download = async () => { if (detail.value.value) { try { await downloadRevision(detail.value.value); } catch (cause) { message.value = cause instanceof Error ? cause.message : 'Download failed.'; } } };
const compare = async () => {
  if (!id || !/^[1-9]\d*$/.test(compareRevision.value)) return; busy.value = true;
  try { comparison.value = await client.request('objects.read', { objectId: id, revision: Number(compareRevision.value) }); }
  catch (cause) { message.value = cause instanceof Error ? cause.message : 'Comparison is unavailable.'; }
  finally { busy.value = false; }
};
const pageLink = computed(() => uiUrl + route('page', { id: id ?? '', ...(historical && revision ? { revision: String(revision) } : {}) }));
const archive = useObjectArchive(client, 'ivy:wiki:archive:' + base.href + ':' + id);
const inheritedArchive = computed(() => !!detail.value.value?.object.effectivelyArchived && !detail.value.value.object.archivedAt);
const archivable = computed(() => !historical && !!id && detail.value.value?.object.contractKey === 'wiki/page' && detail.value.value.revision.contractVersion === '1.0.0' && !dirty.value && (!inheritedArchive.value || !!archive.pending.value));
const setArchived = async () => {
  if (!id || !archivable.value) return;
  const target = archive.pending.value?.archived ?? !detail.value.value!.object.effectivelyArchived;
  if (target && !archive.pending.value && !window.confirm('Archive this page? Child pages will also be hidden until this page is restored.')) return;
  archive.clearError(); const result = await archive.run(id, target);
  if (result) { draft.value = null; message.value = target ? 'Page archived.' : 'Page restored.'; refresh(); window.dispatchEvent(new Event('ivy:objects-changed')); }
};
const deleted = () => {
  sessionStorage.removeItem(key);
  window.dispatchEvent(new Event('ivy:objects-changed'));
  location.hash = route('home');
};
</script>

<template>
  <ToolbarContent>
    <BreadcrumbTrail label="Wiki breadcrumbs" :root="{ label: 'Wiki', href: route('home') }" :items="ancestry" />
  </ToolbarContent>
  <ToolbarContent side="end">
    <span v-if="draft" role="status" class="mr-1 max-w-28 truncate text-xs text-muted-foreground sm:max-w-56">{{ statusLabel }}</span>
    <Button v-if="draft && dirty" variant="ghost" size="sm" class="max-sm:hidden" :disabled="busy || !draft.title.trim() || !!conflict || (!creating && !writable && !draft.operationId)" @click="manualSave">Save page</Button>
    <Button v-if="id && detail.value.value?.object.contractKey === 'wiki/page'" variant="ghost" size="sm" :disabled="uploads.busy.value" @click="openDetails('attachments')"><Paperclip aria-hidden="true" /><span class="hidden sm:inline">Images and files</span><span class="sr-only sm:hidden">Images and files</span></Button>
    <DropdownMenu v-if="detail.value.value">
      <DropdownMenuTrigger as-child><Button variant="ghost" size="icon-sm" aria-label="Page options"><MoreHorizontal aria-hidden="true" /></Button></DropdownMenuTrigger>
      <DropdownMenuContent align="end" class="w-56">
        <DropdownMenuItem v-if="id" @select="copyLink"><Link2 aria-hidden="true" />Copy link</DropdownMenuItem>
        <DropdownMenuItem v-if="writable && id && !dirty" as-child><a :href="route('new', { parent: id })"><FilePlus2 aria-hidden="true" />New subpage</a></DropdownMenuItem>
        <DropdownMenuItem v-if="writable" :disabled="dirty || busy" @select="pageLocation?.open()"><FolderInput aria-hidden="true" />Move page</DropdownMenuItem>
        <DropdownMenuSeparator />
        <DropdownMenuItem v-if="id && detail.value.value.object.contractKey === 'wiki/page'" @select="openDetails('attachments')"><Paperclip aria-hidden="true" />Attachments</DropdownMenuItem>
        <DropdownMenuItem v-if="id" @select="openDetails('history')"><History aria-hidden="true" />Page history</DropdownMenuItem>
        <DropdownMenuItem @select="download"><Download aria-hidden="true" />Download revision</DropdownMenuItem>
        <template v-if="archivable || (!historical && id && detail.value.value.object.contractKey === 'wiki/page')">
          <DropdownMenuSeparator />
          <DropdownMenuItem v-if="archivable" :disabled="archive.busy.value" @select="setArchived">{{ archive.retrying.value ? 'Retry original archive' : detail.value.value.object.effectivelyArchived ? 'Restore page' : 'Archive page' }}</DropdownMenuItem>
          <DropdownMenuItem v-if="!historical && id && detail.value.value.object.contractKey === 'wiki/page'" variant="destructive" :disabled="dirty || busy" @select="deleteOpen = true">Delete permanently…</DropdownMenuItem>
        </template>
      </DropdownMenuContent>
    </DropdownMenu>
  </ToolbarContent>
  <div class="mx-auto w-full max-w-3xl">
    <p v-if="ancestryError" class="mb-3 text-sm text-muted-foreground" role="alert">{{ ancestryError }}</p>
    <RemoteState v-if="!creating" :loading="detail.loading.value" :error="detail.error.value" :has-data="!!detail.value.value" @retry="detail.refresh" />
    <Alert v-if="historical && detail.value.value" class="mb-6"><Clock aria-hidden="true" /><AlertDescription class="flex flex-wrap items-center justify-between gap-2"><span>Historical revision {{ detail.value.value.revision.revision }} · read only</span><a :href="route('page', { id: id ?? '' })" class="font-medium text-foreground underline">Open current version</a></AlertDescription></Alert>
    <Alert v-else-if="detail.value.value?.object.contractKey === 'wiki/page' && (detail.value.value.object.effectivelyArchived || !writable)" class="mb-6"><AlertDescription>{{ inheritedArchive ? 'This page is inside an archived page. Restore its parent first.' : detail.value.value.object.effectivelyArchived ? 'This page is archived and read only.' : 'This page is read only.' }}</AlertDescription></Alert>
    <div v-if="draft || detail.value.value" class="mt-2 mb-4 flex min-w-0 items-start gap-3 sm:mt-8"><span v-if="detail.value.value?.object.icon" class="wiki-title shrink-0" aria-hidden="true">{{ detail.value.value.object.icon }}</span><h1 v-if="draft" :aria-label="creating ? 'Page title' : undefined" :contenteditable="editorLocked ? 'false' : 'plaintext-only'" class="wiki-title min-w-0 flex-1" :class="{ 'is-empty': !draft.title }" data-placeholder="Untitled" ref="titleElement" @input="titleInput" @keydown.enter.prevent="focusDocument"></h1><h1 v-else class="wiki-title min-w-0 flex-1">{{ detail.value.value?.object.name }}</h1></div>
    <PageLocation v-if="detail.value.value?.object.contractKey === 'wiki/page'" :key="detail.value.value.object.name + ':' + detail.value.value.object.parentId" ref="pageLocation" hide-trigger :page="detail.value.value.object" :disabled="!writable || dirty || busy" @changed="refresh" />
    <ObjectDeleteDialog v-if="id && !historical" v-model:open="deleteOpen" :client="client" :object-id="id" :expected-revision="detail.value.value?.object.currentRevision ?? null" :label="'the page “' + (detail.value.value?.object.name ?? 'this page') + '”'" consequence="This also deletes its subpages and attachments." :storage-key="'ivy.wiki.delete:' + base.href + ':' + id" @deleted="deleted" />
    <Alert v-if="message" class="mb-5" role="status"><AlertDescription>{{ message }}</AlertDescription></Alert>
    <Alert v-if="archive.error.value" variant="destructive" class="mb-4" role="alert"><AlertDescription>{{ archive.error.value }}</AlertDescription></Alert>
    <Alert v-if="storageError" variant="destructive" class="mb-4" role="alert"><AlertDescription>{{ storageError }}</AlertDescription></Alert>
    <Alert v-if="imageError" variant="destructive" class="mb-4" role="alert"><AlertDescription class="flex flex-wrap items-center gap-2">{{ imageError }}<Button variant="outline" size="sm" @click="detail.refresh">Retry attachments</Button></AlertDescription></Alert>
    <Alert v-if="attachmentsAttention && !detailsOpen" class="mb-4"><Paperclip aria-hidden="true" /><AlertDescription class="flex flex-wrap items-center justify-between gap-2"><span>An attachment needs your attention.</span><Button variant="outline" size="sm" @click="openDetails('attachments')">Open attachments</Button></AlertDescription></Alert>

    <section v-if="draft" class="wiki-document" aria-label="Page editor">
      <form @submit.prevent="save">
        <p v-if="creating" class="mb-3 text-xs text-muted-foreground">Changes are saved automatically once the page has a title. Images and files can be added after that.</p>
        <p v-else-if="writable" class="mb-3 text-xs text-muted-foreground">Paste or drop images and files here, or choose Images and files. Up to 8 MiB per file.</p>
        <MarkdownEditor ref="editor" :model-value="draft.body" @update:model-value="bodyInput" :trusted-image-urls="trustedImageUrls" :trusted-attachments="trustedAttachments" :upload-file="uploadFile" :disabled="editorLocked" @normalized="editorNormalized" @uploaded="inserted" />
        <p v-if="!creating && !writable && !draft.operationId" class="mt-3 text-sm text-muted-foreground">This page is unavailable or uses an unsupported version. Your draft remains available to copy.</p>
      </form>
      <section v-if="conflict" class="mt-6 rounded-lg border p-4" aria-label="Edit conflict"><h2 class="text-sm font-semibold">This page changed while you were editing</h2><p class="mt-1 text-sm text-muted-foreground">Your draft is retained. Compare it with revision {{ conflict.revision.revision }} before choosing a new save base.</p><RevisionDiff class="mt-4" :before="readableText(conflict)" :after="draft.body" :before-label="'Current revision ' + conflict.revision.revision" after-label="Your draft" /><div class="mt-4 flex flex-wrap gap-2"><Button variant="outline" :disabled="conflict.revision.contractVersion !== '1.0.0'" @click="rebase">Use revision {{ conflict.revision.revision }} as save base</Button><Button variant="ghost" @click="discard">Discard my changes</Button></div><p v-if="conflict.revision.contractVersion !== '1.0.0'" class="mt-3 text-sm">This Wiki release cannot write the current contract version. Your draft remains available to copy.</p></section>
    </section>
    <article v-else-if="detail.value.value" class="wiki-document">
      <ContentView v-if="detail.value.value.object.contractKey === 'wiki/page' && detail.value.value.content.encoding === 'text'" :text="readableText(detail.value.value)" media-type="text/markdown" :maximum-characters="1048576" :trusted-image-urls="trustedImageUrls" :trusted-attachments="trustedAttachments" />
      <section v-else-if="detail.value.value.object.contractKey === 'wiki/attachment'" class="space-y-4" aria-label="Attachment">
        <div class="flex flex-wrap items-center gap-3"><span class="text-sm text-muted-foreground">{{ fileSize(detail.value.value.revision.byteLength) }} · Revision {{ detail.value.value.revision.revision }}</span><Button variant="outline" @click="download"><Download aria-hidden="true" />Download file</Button><Button v-if="detail.value.value.object.ownerObjectId" variant="ghost" as-child><a :href="route('page', { id: detail.value.value.object.ownerObjectId })">Open owning page</a></Button></div>
        <p v-if="mediaLoading" class="text-sm text-muted-foreground" role="status">Loading preview…</p>
        <button v-else-if="attachmentPreview" type="button" class="ivy-image-button" :aria-label="'Enlarge ' + detail.value.value.object.name" @click="previewOpen = true"><img :src="attachmentPreview" :alt="detail.value.value.object.name" class="ivy-embedded-image" /></button>
        <p v-else class="text-sm text-muted-foreground">No inline preview is available for this file type. Download the file to open it in its application.</p>
        <ImagePreview v-model:open="previewOpen" :source="attachmentPreview" :name="detail.value.value.object.name" />
      </section>
      <ContentView v-else :text="readableText(detail.value.value)" :media-type="detail.value.value.revision.mediaType" />
    </article>
  </div>

  <Sheet v-if="id && detail.value.value" v-model:open="detailsOpen">
    <SheetContent class="w-full gap-0 overflow-y-auto sm:max-w-md">
      <SheetHeader><SheetTitle>Page details</SheetTitle><SheetDescription class="sr-only">Link, attachments and revision history of this page.</SheetDescription></SheetHeader>
      <div class="space-y-6 px-4 pb-6">
        <div class="space-y-2"><Label for="page-link">Stable Object link</Label><div class="flex gap-2"><Input id="page-link" :model-value="pageLink" readonly /><Button variant="outline" size="icon" aria-label="Copy link" @click="copyLink"><Copy aria-hidden="true" /></Button></div></div>
        <Tabs v-model="detailsTab">
          <TabsList class="w-full"><TabsTrigger v-if="detail.value.value.object.contractKey === 'wiki/page'" value="attachments">Attachments</TabsTrigger><TabsTrigger value="history">History</TabsTrigger></TabsList>
          <TabsContent v-if="detail.value.value.object.contractKey === 'wiki/page'" value="attachments" force-mount class="data-[state=inactive]:hidden"><WikiAttachments :page-id="id" :writable="writable && !busy && !draft?.operationId" :historical="historical" :references="{ ...detail.value.value.revision.references, ...draft?.attachments }" :image-urls="trustedImageUrls" :uploader="uploads" :upload-file="uploadFile" @uploaded="insertAttachment" @insert="insertExisting" /></TabsContent>
          <TabsContent value="history" class="space-y-4 pt-2">
            <RemoteState :loading="history.loading.value" :error="history.error.value" :has-data="!!history.value.value" @retry="history.refresh" />
            <div class="overflow-hidden rounded-lg border"><a v-for="item in history.value.value?.items ?? []" :key="item.revision" :href="route('page', { id, revision: String(item.revision) })" class="flex flex-col gap-0.5 border-b px-3 py-2 text-sm last:border-b-0 hover:bg-muted/50" :class="{ 'bg-muted/50': revision === item.revision }"><span class="font-medium">Revision {{ item.revision }}</span><span class="text-xs text-muted-foreground">{{ dateLabel(item.createdAt) }} · {{ item.byteLength.toLocaleString() }} bytes</span></a></div>
            <PageControls v-if="history.value.value && (history.value.value.nextCursor || history.page.value > 1)" :count="history.value.value.items.length" :page="history.page.value" :has-next="!!history.value.value.nextCursor" :loading="history.loading.value" @next="history.next" @previous="history.previous" />
            <form v-if="detail.value.value.content.encoding !== 'base64'" class="flex items-end gap-2" @submit.prevent="compare"><div class="flex-1 space-y-2"><Label for="wiki-compare">Compare with revision</Label><Input id="wiki-compare" v-model="compareRevision" type="number" min="1" :max="detail.value.value.object.currentRevision" required /></div><Button type="submit" variant="outline" :disabled="busy">Compare</Button></form>
            <RevisionDiff v-if="comparison" :before="readableText(comparison)" :after="readableText(detail.value.value)" :before-label="'Revision ' + comparison.revision.revision" :after-label="'Revision ' + detail.value.value.revision.revision" />
          </TabsContent>
        </Tabs>
      </div>
    </SheetContent>
  </Sheet>
</template>

<style scoped>
.wiki-title { min-height: 1.2em; overflow-wrap: anywhere; font-size: clamp(1.875rem, 5vw, 2.5rem); font-weight: 700; line-height: 1.2; letter-spacing: -0.02em; outline: none; }
.wiki-title.is-empty::before { content: attr(data-placeholder); color: var(--muted-foreground); pointer-events: none; }
.wiki-title[contenteditable='plaintext-only']:focus { caret-color: var(--foreground); }
</style>
