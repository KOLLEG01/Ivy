import { digest } from '../../../packages/sdk/src/node.js';
import { requireThat } from '../../../packages/sdk/src/node.js';
import type { Chat } from '../../../packages/sdk/src/node.js';
import { validateChat } from '../../../packages/sdk/src/node.js';
import type { ChatStore } from './store.js';
import { chunkBytes } from './evidence-bytes.js';
import { collectionBytes } from './collection-reads.js';

/** Read complete inert reply text from the original Input's immutable artifact revisions. */
export async function readChatResultText(store: ChatStore, inputId: string, result: Chat.Result): Promise<string> {
  validateChat('Result', result);
  requireThat(result.inputId === inputId, 'chat_evidence_mismatch', 'Result text must belong to its original input.');
  const parts: string[] = [], identities = new Set<string>(); let total = 0;
  for (const artifact of result.textParts) {
    requireThat(artifact.object.revision === 1 && artifact.mediaType === 'text/plain' && !identities.has(artifact.object.objectId),
      'chat_evidence_mismatch', 'Result text must retain distinct immutable text parts.'); identities.add(artifact.object.objectId);
    total += artifact.byteLength;
    requireThat(total <= collectionBytes && artifact.byteLength <= chunkBytes, 'chat_collection_limit', 'Complete result text must fit its retained part and total byte bounds.');
    const saved = await store.client.request('objects.read', artifact.object);
    requireThat(saved.object.id === artifact.object.objectId && saved.object.parentId === inputId && !saved.object.effectivelyArchived &&
      saved.object.contractKey === 'chat-bridge/result-text' && saved.revision.revision === artifact.object.revision && saved.revision.contractVersion === '1.0.0' &&
      saved.revision.mediaType === artifact.mediaType && saved.content.encoding === 'text',
      'chat_evidence_mismatch', 'A reply text part must keep its exact original scope, contract and revision.');
    const bytes = Buffer.from(saved.content.value, 'utf8');
    requireThat(bytes.toString('utf8') === saved.content.value && bytes.length === artifact.byteLength && saved.revision.byteLength === bytes.length &&
      digest(bytes) === artifact.contentHash && saved.revision.contentHash === artifact.contentHash,
      'chat_evidence_mismatch', 'Retained reply text differs from its complete original bytes.');
    const text = bytes.toString('utf8');
    requireThat(Buffer.from(text).equals(bytes), 'chat_result_text_invalid', 'Each retained reply part must contain complete lossless UTF-8.'); parts.push(text);
  }
  return parts.join('');
}

/** Deterministic presentation parts; the complete original text remains separately retained. */
export function chatReplyParts(result: Chat.Result, text: string): Array<{ text: string; artifacts: Chat.Artifact[] }> {
  const parts: Array<{ text: string; artifacts: Chat.Artifact[] }> = [], characters: string[] = [];
  for (const character of text) {
    characters.push(character);
    if (characters.length === 16384) { parts.push({ text: characters.join(''), artifacts: [] }); characters.length = 0; }
  }
  if (characters.length) parts.push({ text: characters.join(''), artifacts: [] });
  if (!parts.length) parts.push({ text: 'Native turn ' + result.nativeState + '.', artifacts: [] });
  for (let offset = 0, index = 0; offset < result.textParts.length; offset += 32, index++) {
    if (!parts[index]) parts.push({ text: '', artifacts: [] });
    parts[index]!.artifacts = structuredClone(result.textParts.slice(offset, offset + 32));
  }
  requireThat(parts.length <= 65536, 'chat_collection_limit', 'A complete reply must fit its ordered part count.');
  return parts;
}
