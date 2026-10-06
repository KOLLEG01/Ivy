import { ref } from 'vue';
import { IvyError, newOperationId } from '../../../packages/sdk/src/client';
import { base64, hashBytes } from '../../../packages/ui-client/src/content';
import { base, client } from './runtime';

interface UploadIntent { mutationId: string; name: string; originalName?: string; contentHash: string; byteLength: number }

// Only the mutation identity and file metadata survive a reload. Binary data stays out of browser storage.
export function useWikiUpload(pageId: string | null) {
  const key = 'ivy.wiki.upload:' + base.href + ':' + pageId;
  const pending = ref<UploadIntent | null>(null), busy = ref(false), error = ref<string | null>(null);
  const setError = (message: string | null) => { error.value = message; };
  try {
    const raw = sessionStorage.getItem(key), value = raw && raw.length < 8192 ? JSON.parse(raw) as Partial<UploadIntent> : null;
    if (value && typeof value.mutationId === 'string' && typeof value.name === 'string' && typeof value.contentHash === 'string' && typeof value.byteLength === 'number') pending.value = value as UploadIntent;
  } catch { error.value = 'The previous upload record could not be read.'; }

  const validate = async (file: File) => {
    if (file.size > 8 * 1024 * 1024) throw new Error(file.name + ': Attachments may contain at most 8 MiB.');
    const bytes = new Uint8Array(await file.arrayBuffer()), contentHash = await hashBytes(bytes);
    const intent = pending.value;
    if (intent && (file.name !== (intent.originalName ?? intent.name) || file.size !== intent.byteLength || contentHash !== intent.contentHash)) throw new Error('Reselect the original file with the same name and bytes to retry this upload.');
    return { bytes, contentHash };
  };

  const upload = async (file: File) => {
    if (!pageId) throw new Error('Save the page before adding images or files.');
    if (busy.value) throw new Error('Wait for the current upload to finish.');
    busy.value = true; error.value = null;
    try {
      const { bytes, contentHash } = await validate(file);
      if (!pending.value) {
        const page = await client.request('objects.read', { objectId: pageId });
        if (page.object.contractKey !== 'wiki/page' || page.revision.contractVersion !== '1.0.0' || page.object.effectivelyArchived) throw new Error('The parent page no longer supports new attachments.');
        const names = new Set<string>(); let cursor: string | undefined;
        do {
          const children = await client.request('objects.list', { parentId: pageId, includeArchived: true, limit: 100, ...(cursor ? { cursor } : {}) });
          children.items.forEach(item => names.add(item.name)); cursor = children.nextCursor ?? undefined;
        } while (cursor);
        const originalName = file.name;
        const cleaned = originalName.normalize('NFC').replace(/[\\/\x00-\x1f\x7f]/g, '_').trim().slice(0, 240).toWellFormed();
        const clean = !cleaned || cleaned === '.' || cleaned === '..' ? 'attachment' : cleaned;
        const dot = clean.lastIndexOf('.'), stem = dot > 0 ? clean.slice(0, dot) : clean, extension = dot > 0 ? clean.slice(dot) : '';
        let name = clean, suffix = 2;
        while (names.has(name)) name = `${stem} (${suffix++})${extension}`;
        const intent = { name, originalName, byteLength: file.size, contentHash, mutationId: await newOperationId(client) };
        // Persist before sending so a lost response can only replay the exact same create.
        sessionStorage.setItem(key, JSON.stringify(intent)); pending.value = intent;
      }
      const intent = pending.value;
      const saved = await client.request('objects.write', { mutationId: intent.mutationId, contractVersion: '1.0.0', references: {}, create: { contractKey: 'wiki/attachment', parentId: pageId, ownerObjectId: pageId, name: intent.name }, content: { encoding: 'base64', value: base64(bytes) } });
      if (saved.revision.contentHash !== contentHash || saved.revision.byteLength !== bytes.length) throw new Error('The saved attachment metadata does not match the original file.');
      sessionStorage.removeItem(key); pending.value = null;
      window.dispatchEvent(new Event('ivy:objects-changed'));
      // Pin the file together with the page text when the user saves the draft. Uploading never rewrites the page.
      return { ...saved, content: { encoding: 'base64' as const, value: base64(bytes) } };
    } catch (cause) {
      error.value = cause instanceof Error ? cause.message : 'Upload could not be confirmed.';
      if (cause instanceof IvyError && cause.outcome === 'not_executed') { sessionStorage.removeItem(key); pending.value = null; }
      throw cause;
    } finally { busy.value = false; }
  };
  return { pending, busy, error, setError, validate, upload };
}
