import { hashBytes } from '../../../packages/ui-client/src/content';
import { record } from '../../../packages/ui-client/src/native';
import { client, readDocument } from './runtime';
import type { TaskBoard } from './runtime';
import { canonical } from '../../../packages/sdk/src/client.js';

/** Read the complete immutable observation before showing or downloading native output. */
export async function readNativeOutput(parentId: string, evidence: TaskBoard.ReadEvidenceRef, method: string, signal: AbortSignal) {
  const manifest = await readDocument('native-read-evidence', evidence.artifact.object, parentId, signal);
  const maximumChunks = 16;
  if (manifest.read.revision.contentHash !== evidence.artifact.contentHash || manifest.value.observationId !== evidence.observationId || manifest.value.epoch !== evidence.epoch || manifest.value.method !== method || manifest.value.chunks.length > maximumChunks || manifest.value.byteLength > maximumChunks * 1048576 || manifest.value.byteLength < 1 || manifest.value.chunks.length !== Math.ceil(manifest.value.byteLength / 1048576)) throw new Error('The native output evidence does not match its retained manifest.');
  const chunks: Uint8Array[] = []; let total = 0;
  for (const pin of manifest.value.chunks) {
    const read = await client.request('objects.read', pin.object, { signal });
    if (read.object.effectivelyArchived || read.object.parentId !== parentId || read.object.contractKey !== 'task-board/native-read-evidence-chunk' || read.revision.revision !== pin.object.revision || read.revision.contractVersion !== manifest.read.revision.contractVersion || read.revision.mediaType !== 'application/octet-stream' || read.content.encoding !== 'base64' || read.revision.contentHash !== pin.contentHash || read.revision.byteLength !== pin.byteLength || pin.byteLength !== Math.min(1048576, manifest.value.byteLength - total)) throw new Error('A native output chunk differs from its saved evidence pin.');
    const bytes = Uint8Array.from(atob(read.content.value), character => character.charCodeAt(0));
    if (bytes.byteLength !== pin.byteLength || await hashBytes(bytes) !== pin.contentHash) throw new Error('A native output chunk failed its content check.');
    total += bytes.byteLength; if (total > manifest.value.byteLength) throw new Error('The native output exceeded its declared byte count.'); chunks.push(bytes);
  }
  const body = new Uint8Array(total); let offset = 0; for (const chunk of chunks) { body.set(chunk, offset); offset += chunk.length; }
  if (total !== manifest.value.byteLength || await hashBytes(body) !== manifest.value.contentHash) throw new Error('The complete native output failed its content check.');
  const raw = new TextDecoder('utf-8', { fatal: true }).decode(body), observed = record(JSON.parse(raw));
  if (canonical(observed) !== raw) throw new Error('The native output is not the retained canonical JSON.');
  for (const key of ['observationId', 'callerPrincipalId', 'serviceNodeId', 'nativeVersion', 'nativeExecutableHash', 'catalogHash', 'method', 'requestHash', 'epoch'] as const) {
    if (observed[key] !== manifest.value[key]) throw new Error('The native response differs from its saved observation.');
  }
  return { raw, observed, bytes: total };
}
