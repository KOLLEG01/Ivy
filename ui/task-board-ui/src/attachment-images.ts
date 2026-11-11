import { computed, onBeforeUnmount, ref, watch } from 'vue';
import { markdownImageSources } from '@ivy/ui';
import type { AttachmentInfo } from '@ivy/ui';
import { base64Bytes, hashBytes } from '../../../packages/ui-client/src/content';
import { BlobUrls } from '../../../packages/ui-client/src/blob-urls';
import { attachmentView } from './attachment-view';
import { client } from './runtime';
import type { TaskBoard } from './runtime';

/** Task Markdown embeds an attachment of the same Task by its attachment id. */
export const attachmentSource = (attachmentId: string) => 'attachment:' + attachmentId;
const attachmentParameter = /^attachment:([A-Za-z0-9._-]+)$/;
export const sourceAttachmentId = (source: string) => attachmentParameter.exec(source)?.[1] ?? null;
const linkSources = (text: string) => [...text.matchAll(/(?<!!)\[(?:\\.|[^\]\\])*\]\(([^)\s]+)\)/g)].map(match => match[1]!);

/** The service keeps attachments up to 8 MiB. */
export const checkAttachmentSize = (file: Blob & { name?: string }) => { if (file.size > 8388608) throw new Error((file.name ?? 'The image') + ' is larger than 8 MiB.'); };
/** The uploadAttachment action for one file. */
export function attachmentUpload(taskId: string, expectedRevision: number, attachmentId: string, file: File) {
  checkAttachmentSize(file);
  return { action: 'uploadAttachment' as const, taskId, expectedRevision, attachmentId, filename: file.name,
    mediaType: file.type || 'application/octet-stream', file };
}

/** Hands bytes to the browser as a file download. */
export function saveBytes(bytes: Uint8Array<ArrayBuffer>, filename: string) {
  const url = URL.createObjectURL(new Blob([bytes], { type: 'application/octet-stream' }));
  const anchor = document.createElement('a');
  anchor.href = url; anchor.download = filename;
  document.body.append(anchor); anchor.click(); anchor.remove();
  setTimeout(() => URL.revokeObjectURL(url), 30000);
}

/** Reads an attachment's exact saved bytes and refuses anything that differs from the Task record. */
export async function attachmentBytes(attachment: TaskBoard.TaskAttachment, taskId: string, signal?: AbortSignal): Promise<Uint8Array<ArrayBuffer>> {
  if (!Number.isSafeInteger(attachment.byteLength) || attachment.byteLength < 0 || attachment.byteLength > 8388608)
    throw new Error('The attachment exceeds the supported size.');
  const read = await client.request('objects.read', attachment.object, signal ? { signal } : undefined);
  signal?.throwIfAborted();
  const value = read.content.encoding === 'base64' && read.content.value.length === Math.ceil(attachment.byteLength / 3) * 4 ? base64Bytes(read.content.value) : null;
  if (!value || read.object.contractKey !== 'task-board/attachment' || read.object.parentId !== taskId ||
    read.revision.revision !== attachment.object.revision || read.revision.mediaType !== 'application/octet-stream' ||
    value.byteLength !== attachment.byteLength || read.revision.byteLength !== attachment.byteLength ||
    (await hashBytes(value)) !== attachment.contentHash || read.revision.contentHash !== attachment.contentHash)
    throw new Error('The attachment differs from its saved Task revision.');
  signal?.throwIfAborted();
  return value;
}

/** Verified blob URLs for image attachments, and file details for attachment links, in the given Markdown texts. */
export function useAttachmentImages(taskId: () => string | null, attachments: () => TaskBoard.TaskAttachment[], texts: () => string[]) {
  const urls = ref<Record<string, string>>({});
  const cache = new BlobUrls(source => { const next = { ...urls.value }; delete next[source]; urls.value = next; });
  const pending = new Map<string, Promise<string | null>>();
  let queue: Promise<unknown> = Promise.resolve(), controller = new AbortController();
  let generation = 0;
  const release = () => {
    generation++;
    controller.abort(); controller = new AbortController();
    cache.clear();
    urls.value = {};
    pending.clear();
  };
  /** Reuses a verified image for inline content, attachment cards and the ticket gallery. */
  const load = async (source: string): Promise<string | null> => {
    const retained = cache.get(source);
    if (retained) return retained;
    const id = taskId(), attachmentId = sourceAttachmentId(source);
    if (!id || !attachmentId) return null;
    const attachment = attachments().find(value => value.attachmentId === attachmentId);
    if (!attachment || attachmentView(attachment.filename, attachment.mediaType) !== 'image') return null;
    let operation = pending.get(attachmentId);
    if (!operation) {
      const current = generation, signal = controller.signal;
      operation = queue.then(async () => {
        if (current !== generation) return null;
        const existing = cache.get(source);
        if (existing) return existing;
        const value = await attachmentBytes(attachment, id, signal);
        if (current !== generation) return null;
        const url = cache.put(source, new Blob([value], { type: attachment.mediaType }));
        urls.value = { ...urls.value, [source]: url };
        return url;
      }).catch(cause => {
        if (signal.aborted || current !== generation) return null;
        throw cause;
      }).finally(() => { if (current === generation) pending.delete(attachmentId); });
      queue = operation.catch(() => undefined);
      pending.set(attachmentId, operation);
    }
    return operation;
  };
  watch(taskId, release);
  watch(() => [taskId(), attachments(), texts().join('\n')] as const, async ([id], _old, onCleanup) => {
    if (!id) return;
    const current = generation; let active = true;
    onCleanup(() => { active = false; });
    for (const source of new Set(texts().flatMap(text => markdownImageSources(text)))) {
      if (!active || current !== generation) return;
      try { await load(source); }
      catch { /* Unavailable inline images remain references; the gallery offers an explicit retry. */ }
    }
  }, { immediate: true });
  const files = computed(() => {
    const result: Record<string, AttachmentInfo> = {};
    for (const source of texts().flatMap(text => [...linkSources(text), ...markdownImageSources(text)])) {
      const attachment = attachments().find(value => value.attachmentId === sourceAttachmentId(source));
      if (attachment) result[source] = { name: attachment.filename, byteLength: attachment.byteLength };
    }
    return result;
  });
  /** Shows a just-added image from its local bytes before the Task record lists it. */
  const remember = (source: string, file: Blob) => {
    checkAttachmentSize(file);
    const url = cache.put(source, file);
    urls.value = { ...urls.value, [source]: url };
  };
  onBeforeUnmount(release);
  return { urls, files, remember, load, pin: (source: string | null) => cache.pin(source) };
}
