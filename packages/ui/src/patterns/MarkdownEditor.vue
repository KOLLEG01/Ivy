<script setup lang="ts">
import { onBeforeUnmount, onMounted, ref, watch } from 'vue';
import { CrepeBuilder } from '@milkdown/crepe/builder';
import { blockEdit } from '@milkdown/crepe/feature/block-edit';
import { codeMirror } from '@milkdown/crepe/feature/code-mirror';
import { cursor } from '@milkdown/crepe/feature/cursor';
import { linkTooltip } from '@milkdown/crepe/feature/link-tooltip';
import { listItem } from '@milkdown/crepe/feature/list-item';
import { placeholder as placeholderFeature } from '@milkdown/crepe/feature/placeholder';
import { table } from '@milkdown/crepe/feature/table';
import { toolbar } from '@milkdown/crepe/feature/toolbar';
import { imageSchema, linkSchema } from '@milkdown/kit/preset/commonmark';
import { $view, insert, replaceAll } from '@milkdown/kit/utils';
import { editorViewCtx } from '@milkdown/kit/core';
import { uploadConfig } from '@milkdown/kit/plugin/upload';
import { DOMSerializer, Fragment, Slice } from '@milkdown/kit/prose/model';
import type { Node as ProseNode, Schema } from '@milkdown/kit/prose/model';
import { Paperclip } from '@lucide/vue';
import { languages } from '@codemirror/language-data';
import { renderMermaidPreview } from './mermaid-preview';
import { fileSize } from '../lib/attachments';
import type { AttachmentInfo, UploadedAttachment } from '../lib/attachments';
import ImagePreview from './ImagePreview.vue';
import { Alert, AlertDescription } from '../components/alert';
import '@milkdown/crepe/theme/common/prosemirror.css';
import '@milkdown/crepe/theme/common/reset.css';
import '@milkdown/crepe/theme/common/block-edit.css';
import '@milkdown/crepe/theme/common/code-mirror.css';
import '@milkdown/crepe/theme/common/cursor.css';
import '@milkdown/crepe/theme/common/link-tooltip.css';
import '@milkdown/crepe/theme/common/list-item.css';
import '@milkdown/crepe/theme/common/placeholder.css';
import '@milkdown/crepe/theme/common/toolbar.css';
import '@milkdown/crepe/theme/common/table.css';
import '@milkdown/crepe/theme/frame.css';
import { Button } from '../components/button';

// `field` is the compact form-field presentation; `document` is the full-page writing surface.
const props = defineProps<{ modelValue: string; disabled?: boolean; trustedImageUrls?: Record<string, string>; trustedAttachments?: Record<string, AttachmentInfo>; uploadFile?: (file: File) => Promise<UploadedAttachment>;
  variant?: 'document' | 'field'; placeholder?: string; label?: string }>();
// `normalized` reports how the editor serializes externally supplied Markdown, so callers can tell edits from formatting.
const emit = defineEmits<{ 'update:modelValue': [value: string]; normalized: [value: string, source: string]; uploaded: [] }>();
const root = ref<HTMLElement>(), fileInput = ref<HTMLInputElement>();
const uploadError = ref<string | null>(null), previewOpen = ref(false), preview = ref({ source: '', name: '' });
let crepe: CrepeBuilder | null = null, lastPublished = props.modelValue, replacing = false, edited = false;
// Only report formatting of supplied Markdown when no user input changed it in the meantime.
const markEdited = () => { edited = true; };

const publishCurrentMarkdown = () => {
  queueMicrotask(() => {
    if (!crepe || replacing) return;
    const markdown = crepe.getMarkdown();
    if (markdown === lastPublished) return;
    lastPublished = markdown;
    emit('update:modelValue', markdown);
  });
};

const followInternalLink = (event: MouseEvent) => {
  const anchor = event.target instanceof Element ? event.target.closest<HTMLAnchorElement>('a[href^="#/"]') : null;
  const href = anchor?.getAttribute('href');
  if (!href) return;
  event.preventDefault();
  window.location.hash = href;
};

const attachmentLinkView = $view(linkSchema.mark, () => mark => {
  // Preserve the schema's URL sanitization when adding the file-card presentation.
  const { dom, contentDOM } = DOMSerializer.renderSpec(document, mark.type.spec.toDOM!(mark, true));
  const stop = watch(() => props.trustedAttachments?.[String(mark.attrs.href)], info => {
    if (!(dom instanceof HTMLElement)) return;
    dom.classList.toggle('ivy-attachment-link', !!info);
    if (info) { dom.dataset['fileSize'] = fileSize(info.byteLength); dom.title = info.name + ' · ' + fileSize(info.byteLength); }
    else { delete dom.dataset['fileSize']; dom.title = String(mark.attrs.title || ''); }
  }, { immediate: true });
  return { dom, contentDOM: contentDOM ?? null, destroy: stop };
});

// Lucide "maximize-2" and "x", inlined because node views build plain DOM.
const icon = (paths: string) => `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">${paths}</svg>`;
const enlargeIcon = icon('<path d="M15 3h6v6"/><path d="m21 3-7 7"/><path d="m3 21 7-7"/><path d="M9 21H3v-6"/>');
const removeIcon = icon('<path d="M18 6 6 18"/><path d="m6 6 12 12"/>');

// Images are inline atoms: not editable themselves, so the caret can sit before and after them, and a
// click selects the image so Backspace or Delete removes it. The image is re-rendered only when its own
// verified URL changes, which keeps live refreshes from flashing the document.
const safeImageView = $view(imageSchema.node, () => (initialNode, view, getPos) => {
  let node = initialNode, rendered: string | null = null;
  const dom = document.createElement('span');
  dom.className = 'ivy-image-node'; dom.contentEditable = 'false';
  const open = (url: string, name: string) => { preview.value = { source: url, name }; previewOpen.value = true; };
  const action = (label: string, svg: string, run: () => void) => {
    const button = document.createElement('button'); button.type = 'button'; button.className = 'ivy-image-action';
    button.setAttribute('aria-label', label); button.title = label; button.innerHTML = svg;
    button.onmousedown = event => event.preventDefault();
    button.onclick = event => { event.preventDefault(); event.stopPropagation(); run(); };
    return button;
  };
  const remove = () => {
    const position = getPos();
    if (props.disabled || typeof position !== 'number') return;
    view.dispatch(view.state.tr.delete(position, position + node.nodeSize));
    view.focus();
  };
  const render = () => {
    const source = String(node.attrs.src || ''), url = props.trustedImageUrls?.[source], name = String(node.attrs.alt || 'Image');
    const key = source + '\u0000' + (url ?? '') + '\u0000' + name;
    if (key === rendered) return;
    rendered = key;
    dom.replaceChildren();
    dom.classList.toggle('ivy-image-reference', !url?.startsWith('blob:'));
    if (url?.startsWith('blob:')) {
      const image = document.createElement('img');
      image.className = 'ivy-embedded-image'; image.src = url; image.alt = name; image.draggable = false;
      image.onclick = () => { if (props.disabled) open(url, name); };
      const actions = document.createElement('span'); actions.className = 'ivy-image-actions';
      actions.append(action('Enlarge ' + name, enlargeIcon, () => open(url, name)));
      if (!props.disabled) actions.append(action('Remove image', removeIcon, remove));
      dom.append(image, actions);
    } else dom.textContent = `[${name}]`;
  };
  const stopSource = watch(() => props.trustedImageUrls?.[String(node.attrs.src || '')], render, { immediate: true });
  const stopMode = watch(() => props.disabled, () => { rendered = null; render(); });
  return { dom, ignoreMutation: () => true,
    stopEvent: event => event.target instanceof Element && !!event.target.closest('.ivy-image-action'),
    selectNode: () => dom.classList.add('is-selected'), deselectNode: () => dom.classList.remove('is-selected'),
    update: updated => {
      if (updated.type !== node.type) return false;
      node = updated; render(); return true;
    }, destroy: () => { stopSource(); stopMode(); } };
});

// Uploads files through the host app and returns the image or file-link nodes that embed them.
const uploadedNodes = async (files: File[], schema: Schema) => {
  const nodes: ProseNode[] = []; uploadError.value = null;
  try {
    if (props.disabled) return nodes;
    if (!props.uploadFile) throw new Error('File uploads are not available in this editor.');
    for (const file of files) {
      const value = await props.uploadFile(file);
      const node = value.image ? schema.nodes['image']!.create({ src: value.src, alt: value.name, title: '' })
        : schema.text(value.name, [schema.marks['link']!.create({ href: value.src })]);
      if (nodes.length) nodes.push(schema.nodes['hardbreak']!.create());
      nodes.push(node);
    }
  } catch (cause) { uploadError.value = cause instanceof Error ? cause.message : 'Upload failed. Try again from the attachments panel.'; }
  if (nodes.length) emit('uploaded');
  // Resolve even on failure so Milkdown removes its upload placeholder.
  return nodes;
};

onMounted(async () => {
  if (!root.value) return;
  const initialMarkdown = props.modelValue;
  root.value.addEventListener('input', markEdited, true);
  crepe = new CrepeBuilder({
    root: root.value,
    defaultValue: props.modelValue,
  })
    .addFeature(listItem)
    .addFeature(codeMirror, {
      languages,
      searchPlaceholder: 'Search language…',
      noResultText: 'No language found',
      copyText: 'Copy',
      previewLabel: 'Preview',
      previewLoading: 'Loading diagram…',
      previewToggleText: previewOnly => previewOnly ? 'Edit code' : 'Hide preview',
      renderPreview: renderMermaidPreview,
    })
    .addFeature(linkTooltip, { editButton: 'Edit', removeButton: 'Remove', confirmButton: 'Apply', inputPlaceholder: 'Paste link…' })
    .addFeature(cursor)
    .addFeature(blockEdit, {
        textGroup: { label: 'Text', text: { label: 'Text' }, h1: { label: 'Heading 1' }, h2: { label: 'Heading 2' }, h3: { label: 'Heading 3' }, h4: { label: 'Heading 4' }, h5: { label: 'Heading 5' }, h6: { label: 'Heading 6' }, quote: { label: 'Quote' }, divider: { label: 'Divider' } },
        listGroup: { label: 'Lists', bulletList: { label: 'Bulleted list' }, orderedList: { label: 'Numbered list' }, taskList: { label: 'Task list' } },
        advancedGroup: { label: 'Advanced', image: null, codeBlock: { label: 'Code' }, table: { label: 'Table' }, math: null },
    })
    .addFeature(placeholderFeature, { text: props.placeholder ?? "Write something, or type '/' for commands…", mode: props.variant === 'field' ? 'doc' : 'block' })
    .addFeature(toolbar, { boldLabel: 'Bold', italicLabel: 'Italic', codeLabel: 'Code', linkLabel: 'Link', strikethroughLabel: 'Strikethrough' })
    .addFeature(table);
  crepe.editor.config(ctx => ctx.update(imageSchema.key, previous => context => ({
    ...previous(context),
    // Remark represents an absent image title as null; ProseMirror requires a string.
    parseMarkdown: { match: node => node.type === 'image', runner: (state, node, type) => {
      state.addNode(type, { src: String(node.url ?? ''), alt: String(node.alt ?? ''), title: String(node.title ?? '') });
    } },
    toDOM: node => ['span', { class: 'ivy-image-reference' }, `[${String(node.attrs.alt || 'Image')}]`],
  }))).config(ctx => ctx.update(uploadConfig.key, previous => ({
    ...previous, enableHtmlFileUploader: true,
    uploader: async (files, schema) => uploadedNodes(Array.from(files), schema),
  }))).use(safeImageView).use(attachmentLinkView);
  crepe.on(listener => listener.markdownUpdated((_ctx, markdown) => {
    if (replacing || markdown === lastPublished) return;
    lastPublished = markdown; emit('update:modelValue', markdown);
  }));
  crepe.setReadonly(!!props.disabled);
  await crepe.create();
  if (!edited) emit('normalized', crepe.getMarkdown(), initialMarkdown);
  const editable = root.value.querySelector<HTMLElement>('.ProseMirror');
  editable?.setAttribute('role', 'textbox'); editable?.setAttribute('aria-multiline', 'true');
  editable?.setAttribute('aria-label', props.label ?? 'Markdown');
  root.value.addEventListener('input', publishCurrentMarkdown);
  root.value.addEventListener('click', followInternalLink, true);
});

const insertMarkdown = (markdown: string, focus = true) => {
  if (!crepe || props.disabled) return;
  uploadError.value = null;
  crepe.editor.action(insert(markdown));
  if (focus) crepe.editor.action(ctx => ctx.get(editorViewCtx).focus());
  publishCurrentMarkdown();
};
// Inserts files chosen outside the editor at the caret, like a paste would.
const uploadFiles = async (files: File[]) => {
  if (!crepe || props.disabled || !files.length) return;
  const view = crepe.editor.action(ctx => ctx.get(editorViewCtx));
  const nodes = await uploadedNodes(files, view.state.schema);
  if (!nodes.length || !crepe) return;
  view.dispatch(view.state.tr.replaceSelection(new Slice(Fragment.from(nodes), 0, 0)).scrollIntoView());
  view.focus(); publishCurrentMarkdown();
};
const chooseFiles = (event: Event) => {
  const input = event.target as HTMLInputElement, files = Array.from(input.files ?? []);
  input.value = ''; void uploadFiles(files);
};
defineExpose({ insertMarkdown, uploadFiles });

watch(() => props.disabled, value => crepe?.setReadonly(!!value));
watch(() => props.modelValue, value => {
  if (!crepe || value === lastPublished) return;
  lastPublished = value; replacing = true; edited = false; crepe.editor.action(replaceAll(value, true));
  queueMicrotask(() => { replacing = false; if (crepe && !edited) emit('normalized', crepe.getMarkdown(), value); });
});
onBeforeUnmount(() => {
  root.value?.removeEventListener('input', markEdited, true);
  root.value?.removeEventListener('input', publishCurrentMarkdown);
  root.value?.removeEventListener('click', followInternalLink, true);
  if (crepe) void crepe.destroy();
  crepe = null;
});
</script>

<template>
  <Alert v-if="uploadError" variant="destructive" class="mb-3"><AlertDescription>{{ uploadError }}</AlertDescription></Alert>
  <div v-if="variant === 'field'" class="markdown-field" :class="{ 'is-readonly': disabled }">
    <div ref="root" class="markdown-editor is-field" :class="{ 'is-readonly': disabled }" />
    <div v-if="uploadFile || $slots['actions']" class="flex flex-wrap items-center gap-1 border-t px-1.5 py-1">
      <template v-if="uploadFile">
        <input ref="fileInput" type="file" multiple class="sr-only" tabindex="-1" aria-hidden="true" :disabled="disabled" @change="chooseFiles" />
        <Button type="button" variant="ghost" size="sm" :disabled="disabled" @click="fileInput?.click()"><Paperclip aria-hidden="true" />Add file</Button>
      </template>
      <slot name="actions" />
    </div>
  </div>
  <div v-else ref="root" class="markdown-editor" :class="{ 'is-readonly': disabled }" aria-label="Document" />
  <ImagePreview v-model:open="previewOpen" :source="preview.source" :name="preview.name" />
</template>

<style scoped>
.markdown-editor :deep(.milkdown) {
  --crepe-color-background: transparent;
  --crepe-color-on-background: var(--foreground);
  --crepe-color-surface: var(--card);
  --crepe-color-surface-low: var(--muted);
  --crepe-color-on-surface: var(--foreground);
  --crepe-color-on-surface-variant: var(--muted-foreground);
  --crepe-color-outline: var(--border);
  --crepe-color-primary: var(--foreground);
  --crepe-color-secondary: var(--accent);
  --crepe-color-on-secondary: var(--accent-foreground);
  --crepe-color-inverse: var(--foreground);
  --crepe-color-on-inverse: var(--background);
  --crepe-color-inline-code: var(--foreground);
  --crepe-color-error: var(--destructive);
  --crepe-color-hover: var(--accent);
  --crepe-color-selected: color-mix(in srgb, var(--foreground), transparent 82%);
  --crepe-color-inline-area: var(--muted);
  --crepe-font-title: var(--font-sans);
  --crepe-font-default: var(--font-sans);
  --crepe-font-code: var(--font-mono);
  background: transparent;
}
.markdown-editor :deep(.ProseMirror) { min-height: 22rem; padding: .25rem 0 10rem; font-size: 1rem; line-height: 1.75; }
.markdown-editor :deep(.ProseMirror h1) { font-size: 2rem; font-weight: 650; }
.markdown-editor :deep(.ProseMirror h2) { font-size: 1.55rem; font-weight: 650; }
.markdown-editor :deep(.ProseMirror h3) { font-size: 1.25rem; font-weight: 650; }
.markdown-editor :deep(.ivy-image-reference) { display: inline-flex; max-width: 100%; overflow-wrap: anywhere; border-radius: .25rem; background: var(--muted); padding: .15rem .4rem; color: var(--muted-foreground); font-size: .875rem; }
.markdown-editor :deep(.ivy-embedded-image) { display: inline-block; max-width: 100%; height: auto; max-height: 42rem; border-radius: .375rem; vertical-align: middle; }
.markdown-editor :deep(.ivy-image-node) { position: relative; display: inline-block; min-width: 4.5rem; min-height: 2.5rem; max-width: 100%; vertical-align: middle; border-radius: .375rem; }
.markdown-editor :deep(.ivy-image-node.is-selected) { outline: 2px solid var(--ring); outline-offset: 2px; }
.markdown-editor :deep(.ivy-image-actions) { position: absolute; top: .375rem; right: .375rem; display: flex; gap: .25rem; opacity: 0; transition: opacity .15s; }
.markdown-editor :deep(.ivy-image-node:hover .ivy-image-actions), .markdown-editor :deep(.ivy-image-node.is-selected .ivy-image-actions), .markdown-editor :deep(.ivy-image-actions:focus-within) { opacity: 1; }
.markdown-editor :deep(.ivy-image-action) { display: grid; place-items: center; width: 1.75rem; height: 1.75rem; border-radius: .375rem; border: 1px solid var(--border); background: color-mix(in srgb, var(--background), transparent 10%); color: var(--foreground); cursor: pointer; }
.markdown-editor :deep(.ivy-image-action:hover) { background: var(--accent); }
.markdown-editor :deep(.ivy-image-action svg) { width: 1rem; height: 1rem; }
@media (hover: none) { .markdown-editor :deep(.ivy-image-actions) { opacity: 1; } }
.markdown-editor :deep(.ivy-mermaid-preview) { overflow-x: auto; border-radius: .5rem; background: #fff; padding: 1rem; }
.markdown-editor :deep(.ivy-mermaid-preview svg) { display: block; width: 100%; min-width: 28rem; max-height: 42rem; }
.markdown-editor :deep(.ivy-mermaid-error) { margin: 0; border-radius: .375rem; background: color-mix(in srgb, var(--destructive), transparent 90%); padding: .75rem; color: var(--destructive); font-size: .875rem; }
.markdown-editor.is-readonly :deep(.ProseMirror) { min-height: 0; padding-bottom: 1rem; }
@media (max-width: 767px) { .markdown-editor :deep(.ProseMirror) { min-height: 18rem; padding-bottom: 6rem; } }
.markdown-field { min-width: 0; border: 1px solid var(--input); border-radius: calc(var(--radius) - 2px); box-shadow: 0 1px 2px 0 rgb(0 0 0 / .05); transition: border-color .15s, box-shadow .15s; }
.markdown-field:focus-within { border-color: var(--ring); box-shadow: 0 0 0 3px color-mix(in srgb, var(--ring), transparent 50%); }
.markdown-field.is-readonly { opacity: .6; }
.markdown-editor.is-field :deep(.ProseMirror) { min-height: 6rem; padding: .5rem .75rem; font-size: .875rem; line-height: 1.6; }
.markdown-editor.is-field :deep(.milkdown-block-handle) { display: none; }
</style>
