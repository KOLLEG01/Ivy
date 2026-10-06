import { canonical, digest } from '../../../../packages/sdk/src/node.js';
import { IvyError, requireThat } from '../../../../packages/sdk/src/node.js';
import type { TaskBoard } from '../../../../packages/sdk/src/node.js';
import { TaskBoardStore } from './store.js';
import { managementFrameBytes } from '../../../../packages/sdk/src/node.js';

export const evidenceChunkBytes = 1024 * 1024, evidenceMaximumBytes = managementFrameBytes;
export type EvidenceChunkKey = 'task-board/native-evidence-chunk' | 'task-board/native-read-evidence-chunk';
type ManifestBytes = Pick<TaskBoard.NativeEvidence, 'byteLength' | 'contentHash' | 'chunks'>;

/** Shared byte mechanics only; each evidence type separately verifies its identity and scope. */
export async function saveEvidenceChunks(store: TaskBoardStore, parentId: string, body: Buffer, key: EvidenceChunkKey): Promise<TaskBoard.NativeEvidenceChunk[]> {
  requireThat(body.length > 0 && body.length <= evidenceMaximumBytes, 'task_board_evidence_mismatch', 'Native evidence exceeds its complete byte bound.');
  const contentHash = digest(body), chunks: TaskBoard.NativeEvidenceChunk[] = [];
  for (let offset = 0, index = 0; offset < body.length; offset += evidenceChunkBytes, index++) {
    const bytes = body.subarray(offset, Math.min(body.length, offset + evidenceChunkBytes));
    const name = 'Native evidence chunk ' + contentHash.slice(7) + ' ' + index;
    chunks.push({ object: store.saveLocalBlob(key, name, parentId, bytes), contentHash: digest(bytes), byteLength: bytes.length });
  }
  return chunks;
}

export async function readEvidenceChunks(store: TaskBoardStore, parentId: string, value: ManifestBytes, key: EvidenceChunkKey): Promise<unknown> {
  requireThat(value.byteLength > 0 && value.byteLength <= evidenceMaximumBytes, 'task_board_evidence_mismatch', 'Evidence exceeds its complete byte bound.');
  requireThat(value.chunks.length === Math.ceil(value.byteLength / evidenceChunkBytes), 'task_board_evidence_mismatch', 'Evidence must have the exact bounded ordered chunk count.');
  const bytes: Buffer[] = [];
  for (let index = 0; index < value.chunks.length; index++) {
    const chunk = value.chunks[index]!, part = store.localBlob(key, chunk.object, parentId), expectedLength = Math.min(evidenceChunkBytes, value.byteLength - index * evidenceChunkBytes);
    requireThat(part.length === expectedLength && part.length === chunk.byteLength && digest(part) === chunk.contentHash,
      'task_board_evidence_mismatch', 'Native evidence chunk bytes do not match their ordered immutable length/hash pin.');
    bytes.push(part);
  }
  const body = Buffer.concat(bytes);
  requireThat(body.length === value.byteLength && digest(body) === value.contentHash, 'task_board_evidence_mismatch', 'The complete native evidence differs from the saved body hash or length.');
  let decoded: string, observation: unknown;
  try { decoded = new TextDecoder('utf-8', { fatal: true }).decode(body); observation = JSON.parse(decoded); }
  catch { throw new IvyError('task_board_evidence_mismatch', 'Native evidence is not valid UTF-8 JSON.'); }
  requireThat(canonical(observation, evidenceMaximumBytes) === decoded, 'task_board_evidence_mismatch', 'Native evidence must retain canonical JSON bytes.');
  return observation;
}
