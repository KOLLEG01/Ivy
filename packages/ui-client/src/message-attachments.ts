import { computed, onBeforeUnmount, ref } from 'vue';
import { serviceTools } from '../../sdk/src/client.js';
import type { Agent, BoundTool, RpcClient, Wire } from '../../sdk/src/client.js';
import { backendArguments } from '../../contracts/src/native-backend.js';
import { sameBackend } from './native-backends.js';

/** A file stored on the task's host, referenced by path in a Codex message. */
export interface StagedAttachment { name: string; image: boolean; path: string }
export interface DraftAttachment extends Partial<StagedAttachment> {
  id: string; name: string; image: boolean; previewUrl: string | null; state: 'uploading' | 'ready' | 'failed'; detail?: string;
}
const imageTypes = ['image/png', 'image/jpeg', 'image/gif', 'image/webp'], maximumBytes = 8 * 1024 * 1024;
const maximumFiles = 8, maximumQueuedBytes = 32 * 1024 * 1024;
const base64 = async (file: Blob, check: () => void) => {
  let value = '';
  // A multiple of three keeps each intermediate slice unpadded. Retain only the final base64 wire value.
  for (let offset = 0; offset < file.size; offset += 24576) {
    check();
    const bytes = new Uint8Array(await file.slice(offset, offset + 24576).arrayBuffer());
    check();
    value += btoa(String.fromCharCode(...bytes));
  }
  return value;
};

/** Whether a native input schema offers a Codex user input type such as `localImage`. */
export const supportsInputType = (binding: BoundTool | null | undefined, type: string) =>
  !!binding && JSON.stringify(binding.definition.inputSchema).includes('"' + type + '"');

/** Codex input for a message: the text with attached file paths, then the attached images. */
export function messageInput(text: string, attachments: StagedAttachment[]): Wire.Json[] {
  const files = attachments.filter(value => !value.image);
  const body = [text.trim(), files.map(value => 'Attached file ' + value.name + ': ' + value.path).join('\n')].filter(Boolean).join('\n\n');
  return [...(body ? [{ type: 'text', text: body }] : []), ...attachments.filter(value => value.image).map(value => ({ type: 'localImage', path: value.path }))];
}

/** Draft attachments: each file is staged on the AgentManager host as soon as it is added. */
export function useMessageAttachments(client: RpcClient, node: () => string, backend: () => Agent.BackendIdentity | undefined = () => undefined) {
  const items = ref<DraftAttachment[]>([]);
  const error = ref<string | null>(null);
  type Upload = { id: string; file: File; name: string; node: string; backend?: Agent.BackendIdentity | undefined };
  let pending: Upload[] = [], active: Upload | null = null, stopped = false;
  const update = (id: string, change: Partial<DraftAttachment>) => { items.value = items.value.map(item => item.id === id ? { ...item, ...change } : item); };
  const revoke = (item: DraftAttachment) => { if (item.previewUrl) URL.revokeObjectURL(item.previewUrl); };
  const run = async () => {
    if (active || stopped) return;
    while (pending.length && !stopped) {
      const upload = pending.shift()!;
      active = upload;
      const check = () => {
        if (stopped || !items.value.some(item => item.id === upload.id)) throw new Error('The attachment was removed.');
        if (upload.node !== node() || !sameBackend(upload.backend, backend())) throw new Error('The selected account changed while preparing the file.');
      };
      try {
        check();
        const dataBase64 = await base64(upload.file, check);
        check();
        const staged = await serviceTools(client, upload.node, [{ namespace: 'agent', interfaceVersion: '1.0.0' }])
          .call('agent.stageFile', backendArguments({ name: upload.name, dataBase64 }, upload.backend)) as Agent.StagedFile;
        check();
        if (!sameBackend(staged.backend, upload.backend)) throw new Error('The staged file belongs to another account.');
        update(upload.id, { state: 'ready', path: staged.path });
      } catch (cause) { update(upload.id, { state: 'failed', detail: cause instanceof Error ? cause.message : 'The file could not be added.' }); }
      finally { active = null; }
    }
  };
  const add = (files: File[]) => {
    if (stopped) return;
    error.value = null;
    for (const file of files) {
      if (items.value.length >= maximumFiles) { error.value = 'At most 8 attachments can be added to a message.'; break; }
      const id = crypto.randomUUID(), image = imageTypes.includes(file.type), name = file.name || (image ? 'image.' + file.type.slice(6) : 'file');
      if (file.size > maximumBytes) { items.value = [...items.value, { id, name, image, previewUrl: null, state: 'failed', detail: name + ' is larger than 8 MiB.' }]; continue; }
      if ((active?.file.size ?? 0) + pending.reduce((total, value) => total + value.file.size, 0) + file.size > maximumQueuedBytes) {
        error.value = 'At most 32 MiB of files can wait for upload. Wait for the current files to finish, then add more.'; break;
      }
      items.value = [...items.value, { id, name, image, previewUrl: image ? URL.createObjectURL(file) : null, state: 'uploading' }];
      pending.push({ id, file, name, node: node(), backend: backend() ? { ...backend()! } : undefined });
    }
    void run();
  };
  const remove = (id: string) => { const item = items.value.find(value => value.id === id); if (item) revoke(item); items.value = items.value.filter(value => value.id !== id); pending = pending.filter(value => value.id !== id); error.value = null; };
  /** Restores staged files, for example a message that returns to the draft. */
  const restore = (values: StagedAttachment[]) => {
    items.value = [...items.value, ...values.map(value => ({ ...value, id: crypto.randomUUID(), previewUrl: null, state: 'ready' as const }))];
  };
  const clear = () => { items.value.forEach(revoke); items.value = []; pending = []; error.value = null; };
  const staged = computed<StagedAttachment[]>(() => items.value.filter(item => item.state === 'ready' && item.path).map(item => ({ name: item.name, image: item.image, path: item.path! })));
  onBeforeUnmount(() => { stopped = true; clear(); });
  return { items, error, add, remove, restore, clear, staged,
    uploading: computed(() => items.value.some(item => item.state === 'uploading')),
    failed: computed(() => !!error.value || items.value.some(item => item.state === 'failed')) };
}
