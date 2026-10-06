import { computed, onBeforeUnmount, ref, watch } from 'vue';
import type { AttachmentInfo } from '@ivy/ui';
import { hashBytes } from '../../../packages/ui-client/src/content';
import { attachmentView } from './attachment-view';
import { client } from './runtime';
import type { TaskBoard } from './runtime';

/** Task Markdown embeds an attachment of the same Task by its attachment id. */
export const attachmentSource = (attachmentId: string) => 'attachment:' + attachmentId;
const attachmentParameter = /^attachment:([A-Za-z0-9._-]+)$/;
export const sourceAttachmentId = (source: string) => attachmentParameter.exec(source)?.[1] ?? null;
const imageSources = (text: string) => [...text.matchAll(/!\[(?:\\.|[^\]\\])*\]\(([^)\s]+)\)/g)].map(match => match[1]!);
const linkSources = (text: string) => [...text.matchAll(/(?<!!)\[(?:\\.|[^\]\\])*\]\(([^)\s]+)\)/g)].map(match => match[1]!);

const bytesBase64 = (bytes: Uint8Array) => {
  let value = '';
  for (let offset = 0; offset < bytes.length; offset += 32768) value += String.fromCharCode(...bytes.subarray(offset, offset + 32768));
  return btoa(value);
};
/** The service keeps attachments up to 8 MiB. */
export const checkAttachmentSize = (file: File) => { if (file.size > 8388608) throw new Error(file.name + ' is larger than 8 MiB.'); };
/** The uploadAttachment action for one file. */
export async function attachmentUpload(taskId: string, expectedRevision: number, attachmentId: string, file: File) {
  checkAttachmentSize(file);
  return { action: 'uploadAttachment' as const, taskId, expectedRevision, attachmentId, filename: file.name,
    mediaType: file.type || 'application/octet-stream', bytesBase64: bytesBase64(new Uint8Array(await file.arrayBuffer())) };
}

/** Hands bytes to the browser as a file download. */
export function saveBytes(bytes: Uint8Array, filename: string) {
  const url = URL.createObjectURL(new Blob([new Uint8Array(bytes)], { type: 'application/octet-stream' }));
  const anchor = document.createElement('a');
  anchor.href = url; anchor.download = filename;
  document.body.append(anchor); anchor.click(); anchor.remove();
  setTimeout(() => URL.revokeObjectURL(url), 30000);
}

/** Reads an attachment's exact saved bytes and refuses anything that differs from the Task record. */
export async function attachmentBytes(attachment: TaskBoard.TaskAttachment, taskId: string): Promise<Uint8Array> {
  const read = await client.request('objects.read', attachment.object);
  const value = read.content.encoding === 'base64' ? Uint8Array.from(atob(read.content.value), character => character.charCodeAt(0)) : null;
  if (!value || read.object.contractKey !== 'task-board/attachment' || read.object.parentId !== taskId ||
    read.revision.revision !== attachment.object.revision || read.revision.mediaType !== 'application/octet-stream' ||
    value.byteLength !== attachment.byteLength || read.revision.byteLength !== attachment.byteLength ||
    (await hashBytes(value)) !== attachment.contentHash || read.revision.contentHash !== attachment.contentHash)
    throw new Error('The attachment differs from its saved Task revision.');
  return value;
}

/** Verified blob URLs for image attachments, and file details for attachment links, in the given Markdown texts. */
export function useAttachmentImages(taskId: () => string | null, attachments: () => TaskBoard.TaskAttachment[], texts: () => string[]) {
  const urls = ref<Record<string, string>>({}), byAttachment = new Map<string, string>(), created: string[] = [];
  watch(() => [taskId(), attachments(), texts().join('\n')] as const, async ([id]) => {
    if (!id) return;
    for (const source of new Set(texts().flatMap(imageSources))) {
      const attachmentId = sourceAttachmentId(source);
      if (!attachmentId || urls.value[source]) continue;
      let url = byAttachment.get(attachmentId);
      if (!url) {
        const attachment = attachments().find(value => value.attachmentId === attachmentId);
        if (!attachment || attachmentView(attachment.filename, attachment.mediaType) !== 'image') continue;
        try { url = URL.createObjectURL(new Blob([new Uint8Array(await attachmentBytes(attachment, id))], { type: attachment.mediaType })); }
        catch { continue; }
        byAttachment.set(attachmentId, url); created.push(url);
      }
      urls.value = { ...urls.value, [source]: url };
    }
  }, { immediate: true });
  const files = computed(() => {
    const result: Record<string, AttachmentInfo> = {};
    for (const source of texts().flatMap(linkSources)) {
      const attachment = attachments().find(value => value.attachmentId === sourceAttachmentId(source));
      if (attachment) result[source] = { name: attachment.filename, byteLength: attachment.byteLength };
    }
    return result;
  });
  /** Shows a just-added image from its local bytes before the Task record lists it. */
  const remember = (source: string, file: Blob) => {
    const url = URL.createObjectURL(file), attachmentId = sourceAttachmentId(source);
    created.push(url);
    if (attachmentId) byAttachment.set(attachmentId, url);
    urls.value = { ...urls.value, [source]: url };
  };
  onBeforeUnmount(() => { for (const url of created) URL.revokeObjectURL(url); });
  return { urls, files, remember };
}
