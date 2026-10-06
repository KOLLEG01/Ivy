import { computed, onBeforeUnmount, ref } from 'vue';
import { serviceTools } from '../../sdk/src/client.js';
import type { Agent, BoundTool, RpcClient, Wire } from '../../sdk/src/client.js';

/** A file stored on the task's host, referenced by path in a Codex message. */
export interface StagedAttachment { name: string; image: boolean; path: string }
export interface DraftAttachment extends Partial<StagedAttachment> {
  id: string; name: string; image: boolean; previewUrl: string | null; state: 'uploading' | 'ready' | 'failed'; detail?: string;
}
const imageTypes = ['image/png', 'image/jpeg', 'image/gif', 'image/webp'], maximumBytes = 8 * 1024 * 1024;
const base64 = async (file: Blob) => {
  const bytes = new Uint8Array(await file.arrayBuffer());
  let value = '';
  for (let offset = 0; offset < bytes.length; offset += 32768) value += String.fromCharCode(...bytes.subarray(offset, offset + 32768));
  return btoa(value);
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
export function useMessageAttachments(client: RpcClient, node: () => string) {
  const items = ref<DraftAttachment[]>([]);
  const update = (id: string, change: Partial<DraftAttachment>) => { items.value = items.value.map(item => item.id === id ? { ...item, ...change } : item); };
  const revoke = (item: DraftAttachment) => { if (item.previewUrl) URL.revokeObjectURL(item.previewUrl); };
  const add = (files: File[]) => {
    for (const file of files) {
      const id = crypto.randomUUID(), image = imageTypes.includes(file.type), name = file.name || (image ? 'image.' + file.type.slice(6) : 'file');
      items.value = [...items.value, { id, name, image, previewUrl: image ? URL.createObjectURL(file) : null, state: 'uploading' }];
      void (async () => {
        try {
          if (file.size > maximumBytes) throw new Error(name + ' is larger than 8 MiB.');
          const staged = await serviceTools(client, node(), [{ namespace: 'agent', interfaceVersion: '1.0.0' }])
            .call('agent.stageFile', { name, dataBase64: await base64(file) }) as Agent.StagedFile;
          update(id, { state: 'ready', path: staged.path });
        } catch (cause) { update(id, { state: 'failed', detail: cause instanceof Error ? cause.message : 'The file could not be added.' }); }
      })();
    }
  };
  const remove = (id: string) => { const item = items.value.find(value => value.id === id); if (item) revoke(item); items.value = items.value.filter(value => value.id !== id); };
  /** Restores staged files, for example a message that returns to the draft. */
  const restore = (values: StagedAttachment[]) => {
    items.value = [...items.value, ...values.map(value => ({ ...value, id: crypto.randomUUID(), previewUrl: null, state: 'ready' as const }))];
  };
  const clear = () => { items.value.forEach(revoke); items.value = []; };
  const staged = computed<StagedAttachment[]>(() => items.value.filter(item => item.state === 'ready' && item.path).map(item => ({ name: item.name, image: item.image, path: item.path! })));
  onBeforeUnmount(clear);
  return { items, add, remove, restore, clear, staged,
    uploading: computed(() => items.value.some(item => item.state === 'uploading')),
    failed: computed(() => items.value.some(item => item.state === 'failed')) };
}
