import type { RpcClient } from "../../sdk/src/client.js";
import type { NativeOutputPage } from "./native-output-page.js";

type Scope = readonly [node: string, threadId: string, turnId: string];
interface Entry {
  scope: Scope;
  page: NativeOutputPage;
  at: number;
  size: number;
}

/** A tab-local preview only: every view still rereads its native owner before using cursors. */
export class NativeOutputCache {
  private entries = new Map<string, Entry>();
  private epochs = new Map<string, string>();
  private size = 0;
  constructor(
    private readonly maximumBytes = 4 * 1024 * 1024,
    private readonly maximumEntries = 8,
    private readonly lifetimeMs = 5 * 60_000,
    private readonly now = Date.now,
  ) {}
  private remove(key: string) {
    const entry = this.entries.get(key);
    if (entry) this.size -= entry.size;
    this.entries.delete(key);
  }
  read(scope: Scope): NativeOutputPage | undefined {
    const key = JSON.stringify(scope),
      entry = this.entries.get(key);
    if (!entry) return undefined;
    if (this.now() - entry.at >= this.lifetimeMs) {
      this.remove(key);
      return undefined;
    }
    this.entries.delete(key);
    this.entries.set(key, entry);
    return entry.page;
  }
  write(scope: Scope, page: NativeOutputPage) {
    const key = JSON.stringify(scope);
    this.remove(key);
    if (page.mode === "search") return;
    // Bound retained strings too: a single native item or full-turn download can be large.
    const preview = page.download ? { ...page, download: null } : page;
    const size = JSON.stringify(preview).length * 2;
    if (size > this.maximumBytes) return;
    while (
      this.entries.size &&
      (this.entries.size >= this.maximumEntries ||
        this.size + size > this.maximumBytes)
    )
      this.remove(this.entries.keys().next().value!);
    this.entries.set(key, { scope, page: preview, size, at: this.now() });
    this.size += size;
  }
  observeEpoch(node: string, epoch: string): boolean {
    const previous = this.epochs.get(node);
    this.epochs.set(node, epoch);
    if (!previous || previous === epoch) return false;
    this.invalidate(node);
    return true;
  }
  invalidate(node?: string, threadId?: string) {
    for (const [key, entry] of this.entries)
      if (
        (!node || entry.scope[0] === node) &&
        (!threadId || entry.scope[1] === threadId)
      )
        this.remove(key);
  }
}

const caches = new WeakMap<RpcClient, NativeOutputCache>();
export function nativeOutputCache(client: RpcClient): NativeOutputCache {
  let cache = caches.get(client);
  if (!cache) {
    cache = new NativeOutputCache();
    caches.set(client, cache);
  }
  return cache;
}
