import type { TaskBoard } from '../../contracts/src/generated.js';
import type { RpcClient } from './client.js';

export type CommentEntry = { comment: TaskBoard.Comment; receipt: TaskBoard.CommentDelivery | null };
export type CommentIndex = { object: TaskBoard.ObjectPin; length: number };
export type CommentTree = { kind: 'tree'; root: CommentIndex | null; length: number };
export type CommentNode = { schemaVersion: 1; taskId: string } & (
  { kind: 'comment'; entry: CommentEntry } | { kind: 'page'; children: CommentIndex[] });
export const commentNodeKey = 'task-board/comment-node';

/** Immutable pins permit bounded caching, including historical ticket reads. */
export class CommentReader {
  private readonly cache = new Map<string, { node: CommentNode; bytes: number }>();
  private bytes = 0;
  constructor(readonly client: RpcClient, private readonly maximumBytes = 16 * 1024 * 1024) {}

  remember(pin: TaskBoard.ObjectPin, node: CommentNode): void {
    const key = pin.objectId + '@' + pin.revision;
    if (this.cache.has(key)) return;
    const bytes = JSON.stringify(node).length * 2;
    while (this.bytes + bytes > this.maximumBytes && this.cache.size) {
      const oldest = this.cache.keys().next().value!;
      this.bytes -= this.cache.get(oldest)!.bytes;
      this.cache.delete(oldest);
    }
    if (bytes <= this.maximumBytes) { this.cache.set(key, { node, bytes }); this.bytes += bytes; }
  }

  async node(pin: TaskBoard.ObjectPin, taskId: string, signal?: AbortSignal): Promise<CommentNode> {
    const key = pin.objectId + '@' + pin.revision;
    let node = this.cache.get(key)?.node;
    if (!node) {
      const saved = await this.client.request('objects.read', pin, signal ? { signal } : {});
      if (saved.object.contractKey !== commentNodeKey || saved.revision.contractVersion !== '1.0.0' ||
          saved.object.parentId !== taskId || saved.object.ownerObjectId !== taskId ||
          saved.content.encoding !== 'json' || saved.revision.revision !== pin.revision)
        throw new Error('The conversation snapshot belongs to another ticket.');
      node = saved.content.value as unknown as CommentNode;
      if (node.taskId !== taskId) throw new Error('The conversation snapshot belongs to another ticket.');
      this.remember(pin, node);
    }
    if (node.taskId !== taskId) throw new Error('The conversation snapshot belongs to another ticket.');
    return node;
  }

  /** Range reads follow only intersecting index pages, never scan other Hive objects. */
  async range(tree: CommentTree, taskId: string, start = 0, end = tree.length, signal?: AbortSignal): Promise<CommentEntry[]> {
    if (start < 0 || end > tree.length || start > end || (tree.root?.length ?? 0) !== tree.length)
      throw new Error('The conversation range is invalid.');
    const result: CommentEntry[] = [];
    const pending: { index: CommentIndex; offset: number; depth: number }[] = [];
    const enqueue = (index: CommentIndex, offset: number, depth: number) => {
      if (offset < end && offset + index.length > start) pending.push({ index, offset, depth });
    };
    if (tree.root) enqueue(tree.root, 0, 0);
    while (pending.length) {
      // One SDK read micro-batch at a time also bounds concurrent requests for deep trees.
      const batch = await Promise.all(pending.splice(0, 8).map(async item => {
        if (item.depth > 12 || item.index.length < 1) throw new Error('The conversation index is invalid.');
        return { ...item, node: await this.node(item.index.object, taskId, signal) };
      }));
      for (const { index, node, depth, offset: initial } of batch) {
        let offset = initial;
        if (node.kind === 'comment') {
          if (index.length !== 1 || node.entry.comment.sequence !== offset + 1) throw new Error('The conversation sequence is invalid.');
          result[offset - start] = structuredClone(node.entry);
        } else {
          if (!node.children.length || node.children.reduce((sum, child) => sum + child.length, 0) !== index.length)
            throw new Error('The conversation index is incomplete.');
          for (const child of node.children) { enqueue(child, offset, depth + 1); offset += child.length; }
        }
      }
    }
    return result;
  }
}
